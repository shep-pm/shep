# The shepherd-channel client libraries, plan 2 of 5: the Go module

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development` to implement this task-by-task. Steps use `- [ ]` for tracking.

**Goal:** Ship `github.com/shep-pm/shep-go/channel`, a module a Go app depends on to speak the shepherd channel without hand-rolling the framing, and prove it against the same fixture corpus the Rust crate is pinned to.

**Architecture:** One package, one module, standard library only. A generated `wire.go` holding the two message shapes, hand-written framing over them, and D1's two layers above that. `Open()` hands back a `Conn` with `Recv`, `Send` and no goroutines, for an app that already owns an event loop. `Serve()` is the documented default and a consumer of `Open()`: it owns a reader goroutine and a writer goroutine and guarantees the reply rule the contract asks apps for. The read half is the only thing split by platform: unix reads straight from the conn, Windows polls with `PeekNamedPipe`.

**Tech Stack:** Go 1.24 floor, no third-party dependencies, no `go.sum`. `encoding/json`, `bufio`, `net`, `os/exec`, `sync`, `syscall`.

**Spec:** `docs/brainstorming/specs/2026-09-02-shep-client-libraries-design.md`. This plan implements the "Go, settled 2026-09-06" section, D1 to D9, and the Testing and Windows sections.

**Base:** cut from `origin/main`. Task 1 lands in `shep-pm/shep` on this branch; Tasks 2 to 12 land in a new repository, `shep-pm/shep-go`, which does not exist yet.

## The two measured hazards

Both were measured on 2026-09-06 against go1.26.5, and both change the design rather than decorating it. A plan that ignores either produces a library that passes its own suite while dropping data.

**Hazard 1: Go and Rust disagree about how a float is spelled.** Same value, two encodings:

| side | bytes |
|---|---|
| Rust, serde_json | `{"kind":"metric","name":"rps","value":42.0}` |
| Go, encoding/json | `{"kind":"metric","name":"rps","value":42}` |

Both are valid on the wire, and serde_json reads `42` back as an `f64` without complaint. What it rules out is the test the Rust crate runs: a byte-for-byte comparison of Go's output against the committed fixture cannot pass, and no amount of care in the library will make it pass. So the corpus is checked in two different ways, and the split is deliberate:

- **Decode stays exact.** The committed bytes must produce exactly the expected field values, pointers and all. That is the direction where a mistake loses data.
- **Encode is compared semantically.** Decode both the library's output and the fixture into `map[string]any` and compare those. Still strict about which keys are present, what they are called and what their string values are; tolerant only about how a number is spelled.

**Hazard 2: `omitempty` silently drops a zero metric.** With `Value float64` tagged `json:"value,omitempty"`, a metric of 0 marshals to `{"kind":"metric","name":"rps"}` with no `value` key at all. The shepherd reads a metric with no value as a malformed frame and skips it, so the sample is gone and nothing says so.

The corpus as it stands has no zero-valued metric, so all seven fixtures would pass while that shipped. Two consequences, and both are tasks below:

1. Every Go field that is optional on the wire and has a meaningful zero is a **pointer**. `omitempty` on a pointer omits only `nil`, so a pointer to `0` or to `""` still encodes its key. This is why `ChildMessage` is six pointers and not six values.
2. The Rust corpus gains a zero-valued metric fixture, so every library that reads the corpus is forced to cover the case. That is Task 1, and it happens in `shep-pm/shep` before the Go module exists.

## Where this plan sits

Five plans. This is the second, and it is the first consumer of what plan 1 produced.

| plan | repo | produces |
|---|---|---|
| 1, done | `shep-pm/shep` | `crates/shep-channel`, the fixture corpus, one definition of the wire |
| **2, this one** | `shep-pm/shep-go` | the `channel` module, and the Go spelling of the wire that plan 5 must reproduce |
| 3 | `shep-pm/shep-js` | `@shep-pm/channel` and `@shep-pm/cli` |
| 4 | `shep-pm/shep-py` | `shep-pm` and `shep-cli` |
| 5 | `shep-pm/shep` | the wire emitter and the cross-repo propagation workflow |

Plan 5's acceptance test is that its first run produces a zero diff against `channel/wire.go` as this plan writes it. That makes one file in this plan immutable in a way the rest are not; see the constraint below.

## Global constraints

- **Standard library only.** `go.mod` carries no `require` block and the repository has no `go.sum`. A dependency here is a design decision with an argument attached, not a convenience, and there is currently no argument for one.
- **Module path `github.com/shep-pm/shep-go/channel`, module root `channel/`.** The dog arrives at 0.2.0 as its own module under `dog/`. A module's zip carries only its own subtree, so the licences and the fixtures live inside `channel/` as well as at the repository root. A consumer who runs `go test` on the downloaded module has to find the corpus.
- **`go 1.24` in `go.mod`.** CI tests 1.24 and `stable` across macOS, Linux and Windows. Nothing in the module needs anything newer; the floor is what gets tested, not what gets assumed.
- **`channel/wire.go` is byte-frozen.** Plan 5 emits it from the Rust enums and its acceptance test is a zero diff against this file. Do not reformat it, reorder its fields, rename its identifiers, or edit its comments while working on anything else. Field order is load-bearing on its own terms too: `encoding/json` emits struct fields in declaration order, and three fixtures compare key order.
- **gofmt is the formatter, tabs are the indentation.** Every code block below is already gofmt output. `gofmt -l .` printing a filename is a failure, not a nit.
- **One command shape: `go test ./...`, run from `channel/`.** Add `-race` for anything touching a goroutine, which is most of this module. Do not alternate with per-package invocations; there is one package.
- **Every wait has a deadline.** No test may sleep and hope. A test asserts on an explicit transition: a line arriving, a counter moving, a bounded `select` that fails the test rather than hanging it. The only `time.Sleep` in the whole module is the Windows peek interval, and it is a poll rather than a synchronisation device.
- **Conventional commit subjects on every commit**: `type(scope): summary`, with `!` on anything breaking. release-please reads the individual commits; a subject it cannot parse contributes nothing to the changelog and nothing to the version bump. Accepted types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`.
- **Comments follow IR-47**, the same rule as the Rust side: a comment says only what the code cannot. No history, no rejected alternatives, no paraphrase of the next line. Line comments at four lines, doc comments at six body lines, sentences at sixteen words or fewer. Go's own convention still applies on top: an exported identifier's doc comment starts with its name.
- **The warning prefix is `shep-channel: `, the same string the Rust crate uses.** An operator reading bleats sees one prefix whichever language the app is written in, and greps for one thing.
- **No em dashes or en dashes** in any prose this plan produces, including README text and commit messages.
- **Never write a home-directory path, a real name, or a personal email** into a file, a commit message or a workflow. Repo-relative paths only.

## Plan snippets about existing code are approximations

Every code block below that shows code **being added** is a specification: write it as given unless it does not compile, and say so if it does not. Every block that describes code **already in the tree**, which here means anything under `crates/shep-channel/`, was read on 2026-09-06 and may have moved. Grep for it rather than trusting a line number, and report the difference instead of quietly working around it.

## File structure

```
shep-pm/shep                              existing repo, Task 1 only
  crates/shep-channel/fixtures/
    child-metric-zero.json                new, Task 1
  crates/shep-channel/tests/fixtures.rs   modify, Task 1

shep-pm/shep-go                           new repo, Tasks 2 to 12
  LICENSE-MIT, LICENSE-APACHE             dual, copied from shep
  README.md
  SECURITY.md
  .coderabbit.yaml
  _typos.toml
  .gitignore
  .github/workflows/test.yml              macOS, Linux, Windows, two Go versions
  .github/workflows/commits.yml           conventional subjects, release-please reads them
  .github/workflows/release-please.yml    tag only, no registry
  release-please-config.json
  .release-please-manifest.json
  examples/answers/                       its own module, so the published one stays clean
    go.mod
    main.go
  channel/                                module github.com/shep-pm/shep-go/channel
    go.mod
    LICENSE-MIT, LICENSE-APACHE           the module zip carries only this subtree
    README.md
    doc.go                                the package doc, hand-written
    wire.go                               generated shape, byte-frozen, plan 5 reproduces it
    message.go                            the pointer helper and the three constructors
    action.go                             Action and Fields
    errors.go                             ErrClosed and the malformed-frame error
    session.go                            framing: one line in, one line out
    endpoint.go                           discovery, branching on the variable not the platform
    reader_unix.go                        //go:build !windows
    reader_windows.go                     //go:build windows, the PeekNamedPipe poll
    conn.go                               the low layer: Open, Recv, Send, Close
    outbox.go                             the buffered channel and its two push policies
    dispatch.go                           the handler registry and the reply rule
    shepherd.go                           Serve, the two goroutines, the public surface
    fixtures/*.json                       vendored from shep, eight files
    *_test.go
```

`session.go`, `outbox.go` and `dispatch.go` take an `io.Reader`, an `io.Writer` and plain values rather than the transport. That is what lets the whole of the framing, the drop policy and the reply rule be covered on every platform, including the Windows leg where a named pipe cannot be created without reaching past the standard library.

---

## Task 1: The zero-valued metric the corpus is missing

**Repo: `shep-pm/shep`, this worktree, this branch.**

Hazard 2 in one fixture. Without it, every language's suite can pass while its encoder drops a metric of zero, because nothing in the corpus has a zero in it.

**Files:**
- Modify: `crates/shep-channel/tests/fixtures.rs`
- Create: `crates/shep-channel/fixtures/child-metric-zero.json`

**Interfaces:**
- Produces: `crates/shep-channel/fixtures/child-metric-zero.json`, read verbatim by plans 2, 3 and 4.

- [ ] **Step 1: Add the case to the child table**

In `crates/shep-channel/tests/fixtures.rs`, inside `child_messages_match_their_fixtures`, add a fourth entry to `cases` immediately after `child-metric`:

```rust
        (
            "child-metric-zero",
            ChildMessage::Metric {
                name: "idle".into(),
                value: 0.0,
            },
        ),
```

A different name from `child-metric` on purpose. Two fixtures differing only in a number read as a copy-paste slip; `idle` at `0.0` reads as the case it is.

- [ ] **Step 2: Run it to watch it fail**

Run: `cargo test -p shep-channel --test fixtures`
Expected: FAIL, one panic naming `fixtures/child-metric-zero.json` as missing and telling you to bless.

- [ ] **Step 3: Generate it**

Run: `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test fixtures`
Expected: PASS.

Then read the file. It must be exactly this, one line, no trailing newline:

```
{"kind":"metric","name":"idle","value":0.0}
```

`0.0` rather than `0` is serde's spelling of an `f64`, and it is hazard 1 in the corpus rather than a problem with the fixture. The Go suite compares this one semantically for exactly that reason.

- [ ] **Step 4: Run again without blessing**

Run: `cargo test -p shep-channel --test fixtures`
Expected: PASS, now comparing against the committed file.

- [ ] **Step 5: Prove the new fixture is load-bearing**

Edit `crates/shep-channel/fixtures/child-metric-zero.json`, changing `"value":0.0` to `"value":1.0`. Run the test and confirm `child_messages_match_their_fixtures` fails naming that path. Restore with `git checkout -- crates/shep-channel/fixtures/child-metric-zero.json`.

- [ ] **Step 6: Run the task gate**

Four commands, one at a time, each with `$?` read directly rather than through a pipe:

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```
```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

- [ ] **Step 7: Commit**

```bash
git add crates/shep-channel/fixtures/child-metric-zero.json crates/shep-channel/tests/fixtures.rs docs/writing-plans/plans/2026-09-06-shep-go-channel.md
git commit -m "test(channel): add a zero-valued metric to the wire corpus"
```

The plan file rides in this commit. A planning document opens no pull request of its own; it lands on the branch where the work it describes happens.

---

## Task 2: The repository, the module, and the wire file

**Repo: `shep-pm/shep-go`, created in this task.**

Everything a consumer sees before they read a line of code: the licences, the security policy, the module path, and the one file plan 5 has to reproduce byte for byte.

**Files:**
- Create: the repository, and every file in the "File structure" block above except `channel/fixtures/`, the `_test.go` files, `examples/`, and the three workflows.

**Interfaces:**
- Produces: `channel.Version`, `channel.KindReady`, `channel.KindMetric`, `channel.KindActionReply`, `channel.KindShutdown`, `channel.KindAction`, `channel.ChildMessage`, `channel.ShepherdMessage`.

- [ ] **Step 1: Create the repository and clone it**

```bash
gh repo create shep-pm/shep-go --public --description "Go client for shep: readiness, metrics and custom actions over the shepherd channel"
```

Clone it somewhere outside the shep checkout and work there for the rest of this plan. The first commit goes straight to `main` to create the default branch, since the repository is empty and there is nothing to protect yet; every commit after that goes on a branch called `feat/channel` and lands as one pull request.

Then confirm the CodeRabbit GitHub App covers the new repository. An app installed on the organisation with a hand-picked repository list does not pick up a new one on its own, which is the same failure the shep transfer hit on 2026-08-31.

- [ ] **Step 2: Copy the licences, twice**

Copy `LICENSE-MIT` and `LICENSE-APACHE` verbatim from the shep checkout to the repository root, and then copy both again into `channel/`. The duplication is not an oversight: the Go module proxy serves a zip of the `channel/` subtree only, so a licence at the repository root is invisible to `pkg.go.dev` and to anyone vendoring the module.

- [ ] **Step 3: Write the module manifest**

`channel/go.mod`:

```
module github.com/shep-pm/shep-go/channel

go 1.24
```

No `require` block, and no `go.sum` file will ever be created. If either appears, a dependency crept in and the change that added it is wrong.

- [ ] **Step 4: Write the package doc**

`channel/doc.go`. The package doc lives here rather than in `wire.go` so that the generated file has no prose plan 5 would have to reproduce.

```go
// Package channel speaks the shep shepherd channel: signal readiness,
// emit a metric, answer an action.
//
// Serve is the documented default. Its handle answers an action nobody
// registered. Without a channel every method does nothing, so no call
// site needs a branch. Open is the layer under it, for an app that
// drives its own loop. The contract is docs/shepherd-channel.md in the
// shep repository.
package channel
```

- [ ] **Step 5: Write the wire file**

`channel/wire.go`, exactly as follows. This is the file plan 5 reproduces, so every byte of it is a decision:

```go
// Code generated by shep's wire exporter. DO NOT EDIT.
// Source: crates/shep-channel/src/wire.rs in github.com/shep-pm/shep.

package channel

// Version is the value the shepherd exports as SHEP_CHANNEL_VERSION.
//
// A stamp, not a negotiation. An app can notice a wire it has never
// seen. It cannot ask for a different one.
const Version = "1"

// The kinds carried in every message's "kind" field.
const (
	// KindReady is the child's readiness signal.
	KindReady = "ready"
	// KindMetric is one metric sample from the child.
	KindMetric = "metric"
	// KindActionReply is the child's answer to one action.
	KindActionReply = "action-reply"
	// KindShutdown is the shepherd asking the app to stop.
	KindShutdown = "shutdown"
	// KindAction is the shepherd dispatching one custom action.
	KindAction = "action"
)

// ChildMessage is one line the app writes to the shepherd.
//
// Kind selects which other fields carry meaning. Each of those is a
// pointer. Dropping a zero would lose a metric of 0.
type ChildMessage struct {
	Kind   string   `json:"kind"`
	Name   *string  `json:"name,omitempty"`
	Value  *float64 `json:"value,omitempty"`
	Action *string  `json:"action,omitempty"`
	Body   *string  `json:"body,omitempty"`
	ID     *uint64  `json:"id,omitempty"`
}

// ShepherdMessage is one line the shepherd writes to the app.
//
// Kind selects which other fields carry meaning. An absent Params
// differs from an empty one, so it is a pointer too.
type ShepherdMessage struct {
	Kind   string  `json:"kind"`
	Name   *string `json:"name,omitempty"`
	Params *string `json:"params,omitempty"`
	ID     *uint64 `json:"id,omitempty"`
}
```

Four things about that file are not free choices.

The `Code generated` line is the form the Go toolchain and `gopls` recognise, matching `^// Code generated .* DO NOT EDIT\.$`. Its capitals are a machine-readable token rather than emphasis, which is the one place this repo's comment rules give way. There is a blank line before `package channel` so the marker does not become the package doc.

Field order decides encoded key order. `ChildMessage` puts `Name` and `Value` before `Action` and `Body` so that a metric encodes as `kind, name, value` and a reply as `kind, action, body, id`, which is what the fixtures hold.

Every optional field is a pointer, per hazard 2. `Value` matters most, and `ID` almost as much: an action id of `0` under a plain `uint64` would be dropped from the reply, and the shepherd would fall back to matching by name and order.

The `id` on a `ShepherdMessage` is a pointer even though an action always carries one, because a flat struct cannot say "required" and the alternative is reading a missing id as `0` and echoing that. `session.go` in Task 3 is where absence becomes a refusal.

- [ ] **Step 6: Write the repository scaffolding**

`README.md` at the repository root:

````markdown
# shep-go

Go client for [shep](https://github.com/shep-pm/shep). Signal readiness,
emit a metric, answer an action, all over the channel shep opens for a
supervised app.

```go
import "github.com/shep-pm/shep-go/channel"

shepherd := channel.Serve()

shepherd.OnAction("gc", func(a channel.Action) string {
	runtime.GC()
	return "collected"
})
shepherd.OnShutdown(server.GracefulStop)
if err := shepherd.Ready(); err != nil {
	log.Print(err)
}
shepherd.Metric("rps", 4200)
```

```
go get github.com/shep-pm/shep-go/channel
```

Ask for a channel in the Flockfile with `channel = true`, or get one from
`wait_ready` or `shutdown_with_message`.

Without one, every call above does nothing and the app runs unchanged, so
there is nothing to branch on. An action name you never registered still
gets a reply, which is the part an app is most likely to get wrong: to the
shepherd, silence from an app thinking hard and silence from an app that
never understood the question look the same, and only `action_timeout`
running out tells them apart.

`channel.Open()` is the layer underneath, for an app that already runs its
own event loop: a `Conn` with `Recv`, `Send` and no goroutines. Both go
through the same door, so pick one and never both.

Standard library only. The dog client lands at 0.2.0 as its own module.

Dual licensed under MIT or Apache-2.0.
````

`channel/README.md` is four lines pointing at the root one, so the module zip is not licence-and-code with nothing to read:

````markdown
# channel

The shep shepherd channel for Go: readiness, metrics and custom actions.

Documentation: <https://pkg.go.dev/github.com/shep-pm/shep-go/channel>.
Repository, licences and contributing: <https://github.com/shep-pm/shep-go>.
````

`SECURITY.md`: copy shep's and cut it to what is true here. This module opens no socket, binds no port, runs no server and holds no secret. What it does do is take a descriptor named by the environment and write JSON to it, so the section worth keeping is the premise: the shepherd names a descriptor it opened for this process, and the module refuses anything below 3 rather than adopting the app's own standard streams. Keep shep's disclaimer paragraph and its reporting instructions verbatim.

`_typos.toml`:

```toml
# Exceptions for `crate-ci/typos` (CI job `typos`).
# Every entry here was read in context, not defaulted.

[default.extend-words]
# The wire's own spelling, in every fixture and every kind constant.
kinds = "kinds"
```

Start it as close to empty as it can be and add an entry only when the job actually fires, each with the sentence saying why.

`.coderabbit.yaml`: copy shep's, then cut every `path_instructions` entry that names a Rust path and replace them with one for `channel/**/*.go` saying that the module is standard-library only by design, that `wire.go` is generated and byte-frozen, and that a suggestion to add a dependency should say which of those two it is arguing with. Keep `profile: assertive` and the release-pull-request exclusion.

`.gitignore`:

```
# Go build and coverage output. Nothing in this repo is generated into the
# tree except wire.go, which is committed.
*.exe
*.test
*.out
```

- [ ] **Step 7: Verify the module builds and is formatted**

From `channel/`, one at a time:

```bash
gofmt -l .
```
Expected: no output. Any filename printed is a failure; run `gofmt -w` and read the diff before accepting it.

```bash
go vet ./...
```
Expected: EXIT=0.

```bash
go build ./...
```
Expected: EXIT=0.

- [ ] **Step 8: Commit**

Two commits, because they are two things.

```bash
git add LICENSE-MIT LICENSE-APACHE README.md SECURITY.md .coderabbit.yaml _typos.toml .gitignore
git commit -m "chore: scaffold the shep-go repository"
```

```bash
git add channel
git commit -m "feat(channel): add the wire types and the module manifest"
```

---

## Task 3: Framing, and the message shapes above it

**Repo: `shep-pm/shep-go`.**

One line in, one line out, plus the three small types the rest of the module is written in terms of. All of it independent of any transport, so all of it is covered on every platform.

**Files:**
- Create: `channel/message.go`, `channel/action.go`, `channel/errors.go`, `channel/session.go`
- Create: `channel/session_test.go`, `channel/action_test.go`

**Interfaces:**
- Consumes: `ChildMessage`, `ShepherdMessage` and the kind constants from Task 2.
- Produces, exported: `channel.Action`, `channel.Action.Fields`, `channel.NewReady`, `channel.NewMetric`, `channel.NewReply`, `channel.ErrClosed`, `channel.ErrMalformed`.
- Produces, package-internal: `ptr`, `encodeLine`, `writeMessage`, `readMessage`, `decodeMessage`.

The three constructors are exported and `ptr` is not. An app on Task 6's low layer has to build a `ChildMessage` to send one, and every optional field of one is a pointer, so without them it cannot. Exporting `ptr` instead would put a bare generic helper on the package surface to work around a type this package chose.

- [ ] **Step 1: Write the failing framing tests**

`channel/session_test.go`:

```go
package channel

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"strings"
	"testing"
)

func readerOver(text string) *bufio.Reader {
	return bufio.NewReader(strings.NewReader(text))
}

func TestReadsTwoMessagesFromOneStream(t *testing.T) {
	reader := readerOver("{\"kind\":\"shutdown\"}\n{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n")

	first, err := readMessage(reader)
	if err != nil {
		t.Fatalf("first message: %v", err)
	}
	if first.Kind != KindShutdown {
		t.Fatalf("first message is %q, want %q", first.Kind, KindShutdown)
	}

	second, err := readMessage(reader)
	if err != nil {
		t.Fatalf("second message: %v", err)
	}
	if second.Kind != KindAction || *second.Name != "gc" || *second.ID != 7 {
		t.Fatalf("second message is %+v", second)
	}
	if second.Params != nil {
		t.Fatalf("an action with no params decoded Params as %q", *second.Params)
	}

	if _, err := readMessage(reader); !errors.Is(err, io.EOF) {
		t.Fatalf("end of stream reported %v, want io.EOF", err)
	}
}

// The Windows transport is a byte-mode pipe, so a peer there may write
// \r\n.
func TestACarriageReturnBeforeTheNewlineIsTolerated(t *testing.T) {
	message, err := readMessage(readerOver("{\"kind\":\"shutdown\"}\r\n"))
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if message.Kind != KindShutdown {
		t.Fatalf("kind is %q, want %q", message.Kind, KindShutdown)
	}
}

// The shepherd skips a bad frame and keeps reading. This side has to
// match, or the two halves disagree about what one bad line costs.
func TestAMalformedLineIsRecoverable(t *testing.T) {
	reader := readerOver("not json\n{\"kind\":\"shutdown\"}\n")

	if _, err := readMessage(reader); !errors.Is(err, ErrMalformed) {
		t.Fatalf("a bad line reported %v, want ErrMalformed", err)
	}
	message, err := readMessage(reader)
	if err != nil {
		t.Fatalf("the reader did not resume after a bad line: %v", err)
	}
	if message.Kind != KindShutdown {
		t.Fatalf("kind is %q, want %q", message.Kind, KindShutdown)
	}
}

// A flat struct decodes any object at all. The kind check is what stands
// between an unknown message and a nil dereference.
func TestAnUnknownKindIsMalformed(t *testing.T) {
	_, err := readMessage(readerOver("{\"kind\":\"stampede\"}\n"))
	if !errors.Is(err, ErrMalformed) {
		t.Fatalf("an unknown kind reported %v, want ErrMalformed", err)
	}
	if !strings.Contains(err.Error(), "stampede") {
		t.Fatalf("the refusal does not name the kind: %v", err)
	}
}

func TestAnActionMissingItsNameOrIDIsMalformed(t *testing.T) {
	for _, line := range []string{
		"{\"kind\":\"action\",\"id\":7}\n",
		"{\"kind\":\"action\",\"name\":\"gc\"}\n",
	} {
		if _, err := readMessage(readerOver(line)); !errors.Is(err, ErrMalformed) {
			t.Fatalf("%s reported %v, want ErrMalformed", strings.TrimSpace(line), err)
		}
	}
}

func TestWritesOneLinePerMessageWithATrailingNewline(t *testing.T) {
	var out bytes.Buffer
	if err := writeMessage(&out, NewReady()); err != nil {
		t.Fatalf("write ready: %v", err)
	}
	if err := writeMessage(&out, NewMetric("rps", 42)); err != nil {
		t.Fatalf("write metric: %v", err)
	}
	want := "{\"kind\":\"ready\"}\n{\"kind\":\"metric\",\"name\":\"rps\",\"value\":42}\n"
	if out.String() != want {
		t.Fatalf("wrote %q, want %q", out.String(), want)
	}
}

// json.Marshal escapes <, > and & by default and serde does not. A reply
// body is free-form app text, so the two would drift on ordinary input.
func TestAReplyBodyKeepsItsAngleBracketsVerbatim(t *testing.T) {
	var out bytes.Buffer
	if err := writeMessage(&out, NewReply("gc", "freed <b>4</b> pages", ptr(uint64(7)))); err != nil {
		t.Fatalf("write reply: %v", err)
	}
	if !strings.Contains(out.String(), "freed <b>4</b> pages") {
		t.Fatalf("the body was escaped: %s", out.String())
	}
}
```

`channel/action_test.go`:

```go
package channel

import (
	"reflect"
	"testing"
)

func TestFieldsSplitsParamsOnWhitespace(t *testing.T) {
	action := Action{Name: "gc", Params: ptr("  now   please ")}
	if got := action.Fields(); !reflect.DeepEqual(got, []string{"now", "please"}) {
		t.Fatalf("Fields is %#v", got)
	}
}

// An absent params and an empty one are different messages. Neither may
// panic here.
func TestFieldsIsEmptyForAbsentAndEmptyParams(t *testing.T) {
	if got := (Action{Name: "gc"}).Fields(); len(got) != 0 {
		t.Fatalf("Fields on absent params is %#v", got)
	}
	if got := (Action{Name: "gc", Params: ptr("")}).Fields(); len(got) != 0 {
		t.Fatalf("Fields on empty params is %#v", got)
	}
}

// The shepherd never reads params, so an app can pass JSON or anything
// carrying a space. Fields is a helper beside that, not a grammar on it.
func TestParamsSurvivesWhereFieldsWouldDestroyIt(t *testing.T) {
	action := Action{Name: "set", Params: ptr(`{"level":"debug and loud"}`)}
	if *action.Params != `{"level":"debug and loud"}` {
		t.Fatalf("Params is %q", *action.Params)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test ./...`
Expected: FAIL to build, every identifier the tests name is undefined.

- [ ] **Step 3: Write the message helpers**

`channel/message.go`:

```go
package channel

// ptr returns a pointer to v.
//
// Every optional field on the wire is a pointer. omitempty then drops an
// absent value and never a zero one.
func ptr[T any](v T) *T { return &v }

// NewReady builds the readiness signal.
func NewReady() ChildMessage {
	return ChildMessage{Kind: KindReady}
}

// NewMetric builds one metric sample.
func NewMetric(name string, value float64) ChildMessage {
	return ChildMessage{Kind: KindMetric, Name: ptr(name), Value: ptr(value)}
}

// NewReply builds the answer to one action.
//
// id is echoed verbatim, including its absence. The shepherd matches a
// reply by id, and falls back to name and order without one.
func NewReply(action, body string, id *uint64) ChildMessage {
	return ChildMessage{Kind: KindActionReply, Action: ptr(action), Body: ptr(body), ID: id}
}
```

`channel/action.go`:

```go
package channel

import "strings"

// Action is one action the shepherd dispatched to this app.
type Action struct {
	// Name is the action name the trigger asked for.
	Name string
	// Params is the argument text, nil when the trigger carried none.
	//
	// A pointer because the wire omits the key entirely: an absent
	// params and an empty one are different messages.
	Params *string
}

// Fields splits Params on whitespace, and is empty when there were none.
//
// The shepherd passes params through as one opaque string. An app can
// send JSON, or anything carrying a space. This is where a word split
// belongs, beside that string rather than in place of it.
func (a Action) Fields() []string {
	if a.Params == nil {
		return nil
	}
	return strings.Fields(*a.Params)
}
```

`channel/errors.go`:

```go
package channel

import "errors"

// ErrClosed means the channel is closed and the message was not sent.
//
// From Ready when the shepherd has gone away, and from a Conn whose
// Close has run.
var ErrClosed = errors.New("shep channel closed")

// ErrMalformed is one line the reader could not use.
//
// Recoverable. The next read resumes at the next line. That is what the
// shepherd does with a bad frame too.
var ErrMalformed = errors.New("malformed shepherd-channel frame")
```

- [ ] **Step 4: Write the framing**

`channel/session.go`:

```go
package channel

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
)

// readMessage reads one newline-delimited message.
//
// io.EOF with no bytes is the shepherd closing its end. A bad line is
// ErrMalformed, and the next call resumes at the line after it.
func readMessage(reader *bufio.Reader) (ShepherdMessage, error) {
	line, err := reader.ReadBytes('\n')
	if len(line) == 0 {
		if err == nil {
			err = io.EOF
		}
		return ShepherdMessage{}, err
	}
	// A final line with no newline arrives here with err set. bufio
	// stores that error for the next call. Dropping it now costs
	// nothing, and the last frame is not lost.
	return decodeMessage(line)
}

// decodeMessage turns one line into a message the reader can act on.
//
// The kind check is not a formality. A flat struct unmarshals any JSON
// object at all. This is what makes an action's Name and ID safe to
// dereference.
func decodeMessage(line []byte) (ShepherdMessage, error) {
	trimmed := bytes.TrimRight(line, "\r\n")
	if len(bytes.TrimSpace(trimmed)) == 0 {
		return ShepherdMessage{}, fmt.Errorf("%w: empty line", ErrMalformed)
	}
	var message ShepherdMessage
	if err := json.Unmarshal(trimmed, &message); err != nil {
		return ShepherdMessage{}, fmt.Errorf("%w: %s", ErrMalformed, err)
	}
	switch message.Kind {
	case KindShutdown:
		return message, nil
	case KindAction:
		if message.Name == nil || message.ID == nil {
			return ShepherdMessage{}, fmt.Errorf("%w: an action needs both name and id", ErrMalformed)
		}
		return message, nil
	default:
		return ShepherdMessage{}, fmt.Errorf("%w: unknown kind %q", ErrMalformed, message.Kind)
	}
}

// encodeLine encodes one message, with no trailing newline.
//
// HTML escaping is off. json.Marshal would write \u003c for a < in
// a reply body, and serde writes it verbatim.
func encodeLine(message any) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(message); err != nil {
		return nil, err
	}
	return bytes.TrimRight(buffer.Bytes(), "\n"), nil
}

// writeMessage writes one message and its newline.
func writeMessage(writer io.Writer, message ChildMessage) error {
	line, err := encodeLine(message)
	if err != nil {
		return err
	}
	_, err = writer.Write(append(line, '\n'))
	return err
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `go test ./...`
Expected: PASS.

- [ ] **Step 6: Prove the kind check is not vacuous**

In `decodeMessage`, replace the whole `switch` with `return message, nil`. Run `go test ./...` and confirm `TestAnUnknownKindIsMalformed` and `TestAnActionMissingItsNameOrIDIsMalformed` both fail. Restore.

- [ ] **Step 7: Commit**

```bash
git add channel/message.go channel/action.go channel/errors.go channel/session.go channel/session_test.go channel/action_test.go
git commit -m "feat(channel): frame one message in each direction"
```

---

## Task 4: The vendored corpus, and both directions over it

**Repo: `shep-pm/shep-go`.**

The eight files from Task 1's corpus, read by the Go suite exactly as they were written by Rust's serde impls. Hazard 1 decides how each direction is compared, and hazard 2 gets a test of its own that does not depend on the corpus at all.

**Files:**
- Create: `channel/fixtures/*.json`, eight files copied from `crates/shep-channel/fixtures/`
- Create: `channel/fixtures_test.go`

**Interfaces:**
- Consumes: `encodeLine` from Task 3, and the wire types from Task 2.

- [ ] **Step 1: Vendor the corpus**

Copy every file from the shep checkout's `crates/shep-channel/fixtures/` into `channel/fixtures/`. Eight files after Task 1:

```
child-ready.json
child-metric.json
child-metric-zero.json
child-action-reply.json
child-action-reply-id.json
shepherd-shutdown.json
shepherd-action.json
shepherd-action-params.json
```

Each is one line with no trailing newline. Verify that with `wc -c` against the shep copies rather than trusting the copy: a stray newline appended by an editor would change nothing about correctness and everything about the byte comparison plan 5 eventually runs.

The directory is `fixtures/` rather than Go's conventional `testdata/`. Four repositories vendor the same corpus from the same source, and one path across all of them is worth more than one repository's convention. It costs nothing: a directory with no `.go` files is skipped by the build and carried in the module zip either way.

- [ ] **Step 2: Write the failing fixture tests**

`channel/fixtures_test.go`:

```go
package channel

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"
)

// childCases maps a fixture name to the value its bytes mean.
var childCases = map[string]ChildMessage{
	"child-ready":           {Kind: KindReady},
	"child-metric":          {Kind: KindMetric, Name: ptr("rps"), Value: ptr(42.0)},
	"child-metric-zero":     {Kind: KindMetric, Name: ptr("idle"), Value: ptr(0.0)},
	"child-action-reply":    {Kind: KindActionReply, Action: ptr("gc"), Body: ptr("ok")},
	"child-action-reply-id": {Kind: KindActionReply, Action: ptr("gc"), Body: ptr("ok"), ID: ptr(uint64(7))},
}

// shepherdCases maps a fixture name to the value its bytes mean.
var shepherdCases = map[string]ShepherdMessage{
	"shepherd-shutdown":      {Kind: KindShutdown},
	"shepherd-action":        {Kind: KindAction, Name: ptr("gc"), ID: ptr(uint64(7))},
	"shepherd-action-params": {Kind: KindAction, Name: ptr("set-log-level"), Params: ptr("debug"), ID: ptr(uint64(8))},
}

func fixtureBytes(t *testing.T, name string) []byte {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("fixtures", name+".json"))
	if err != nil {
		t.Fatalf("read the fixture: %v", err)
	}
	return raw
}

// asFields decodes one line into its keys and values. Two encodings of
// one message then compare equal however each side spells a number.
func asFields(t *testing.T, what string, raw []byte) map[string]any {
	t.Helper()
	var fields map[string]any
	if err := json.Unmarshal(raw, &fields); err != nil {
		t.Fatalf("%s is not a JSON object: %v", what, err)
	}
	return fields
}

// Fails when the vendored corpus gains or loses a file. A new fixture
// upstream is a case this suite has to decide about. Ignoring it because
// no test names it is not an option.
func TestEveryVendoredFixtureIsCovered(t *testing.T) {
	paths, err := filepath.Glob(filepath.Join("fixtures", "*.json"))
	if err != nil {
		t.Fatalf("glob the corpus: %v", err)
	}
	var onDisk []string
	for _, path := range paths {
		onDisk = append(onDisk, strings.TrimSuffix(filepath.Base(path), ".json"))
	}
	var covered []string
	for name := range childCases {
		covered = append(covered, name)
	}
	for name := range shepherdCases {
		covered = append(covered, name)
	}
	sort.Strings(onDisk)
	sort.Strings(covered)
	if !reflect.DeepEqual(onDisk, covered) {
		t.Fatalf("the corpus holds %v and this suite covers %v", onDisk, covered)
	}
}

// The decode direction is exact: these bytes must produce exactly these
// values, pointers and all.
func TestFixturesDecodeToTheExpectedValues(t *testing.T) {
	for name, want := range childCases {
		var got ChildMessage
		if err := json.Unmarshal(fixtureBytes(t, name), &got); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("%s decoded to %+v, want %+v", name, got, want)
		}
	}
	for name, want := range shepherdCases {
		var got ShepherdMessage
		if err := json.Unmarshal(fixtureBytes(t, name), &got); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("%s decoded to %+v, want %+v", name, got, want)
		}
	}
}

// The encode direction is compared key by key rather than byte for byte.
// Go writes 42 where serde writes 42.0. Both are valid, and no care here
// would close that gap.
func TestFixturesEncodeToTheSameMessage(t *testing.T) {
	check := func(name string, value any) {
		t.Helper()
		encoded, err := encodeLine(value)
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		got := asFields(t, "the encoded "+name, encoded)
		want := asFields(t, "the committed "+name, fixtureBytes(t, name))
		if !reflect.DeepEqual(got, want) {
			t.Fatalf("%s encoded to %v, want %v", name, got, want)
		}
	}
	for name, value := range childCases {
		check(name, value)
	}
	for name, value := range shepherdCases {
		check(name, value)
	}
}

// Hazard 2, independent of the corpus. With a plain float64 field, a
// metric of 0 marshals with no value key. The sample is gone.
func TestAZeroMetricKeepsItsValueOnTheWire(t *testing.T) {
	encoded, err := encodeLine(NewMetric("idle", 0))
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	fields := asFields(t, "the encoded metric", encoded)
	value, present := fields["value"]
	if !present {
		t.Fatalf("a metric of zero encoded without a value key: %s", encoded)
	}
	if value != float64(0) {
		t.Fatalf("value is %#v, want 0", value)
	}
}

// Ties the corpus to the constructors the library sends, not to literals
// only this file writes.
func TestTheConstructorsProduceTheFixtureValues(t *testing.T) {
	cases := map[string]ChildMessage{
		"child-ready":           NewReady(),
		"child-metric":          NewMetric("rps", 42),
		"child-metric-zero":     NewMetric("idle", 0),
		"child-action-reply":    NewReply("gc", "ok", nil),
		"child-action-reply-id": NewReply("gc", "ok", ptr(uint64(7))),
	}
	for name, built := range cases {
		if !reflect.DeepEqual(built, childCases[name]) {
			t.Fatalf("%s built %+v, want %+v", name, built, childCases[name])
		}
	}
}
```

- [ ] **Step 3: Run to verify it passes**

Run: `go test ./...`
Expected: PASS.

If `TestEveryVendoredFixtureIsCovered` fails, the copy in Step 1 was incomplete or Task 1 did not land. Fix the corpus rather than the test.

- [ ] **Step 4: Prove hazard 2's guard is not vacuous**

Two mutations, because the field and the constructor are separate guards with separate tests.

First the constructor. In `NewMetric`, make the pointer conditional, so a zero encodes the way `omitempty` over a plain `float64` would:

```go
func NewMetric(name string, value float64) ChildMessage {
	message := ChildMessage{Kind: KindMetric, Name: ptr(name)}
	if value != 0 {
		message.Value = ptr(value)
	}
	return message
}
```

Run `go test ./...`. Expected: `TestAZeroMetricKeepsItsValueOnTheWire` fails saying a metric of zero encoded without a value key, and `TestTheConstructorsProduceTheFixtureValues` fails on `child-metric-zero`. Not `TestFixturesEncodeToTheSameMessage`, which encodes the struct literals in `childCases` rather than anything a constructor built. Tying those two together is the whole job of that third test, and this mutation is where it earns its place.

Restore with `git checkout -- channel/message.go`.

Then the field itself, which is hazard 2 as the section above states it. Dropping the pointer does not compile on its own, so every site that dereferences `Value` moves with it. Three of them exist at this point in the plan: `Value *float64` becomes `Value float64` in `channel/wire.go`, `Value: ptr(value)` becomes `Value: value` in `NewMetric`, and `Value: ptr(42.0)` and `Value: ptr(0.0)` become `Value: 42.0` and `Value: 0.0` in `childCases`. Task 7 adds a fourth, `*message.Value` in `channel/outbox_test.go`, so re-running this mutation later takes that one too.

Expected: `TestAZeroMetricKeepsItsValueOnTheWire` fails again, and `TestFixturesEncodeToTheSameMessage` fails on `child-metric-zero` reporting `map[kind:metric name:idle]` against a want that carries `value:0`. Both, not one: the corpus catches it and so does the standalone guard, and each was written to survive the other being deleted.

Restore with `git checkout -- channel/wire.go channel/message.go channel/fixtures_test.go`. `wire.go` is byte-frozen, so confirm `git diff` is empty afterwards.

- [ ] **Step 5: Prove the decode direction is exact**

In `childCases`, change `"child-metric"`'s value from `ptr(42.0)` to `ptr(43.0)`. Confirm `TestFixturesDecodeToTheExpectedValues` fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add channel/fixtures channel/fixtures_test.go
git commit -m "test(channel): read the vendored wire corpus in both directions"
```

---

## Task 5: Discovery, and the two read halves

**Repo: `shep-pm/shep-go`.**

Where the channel is, how it opens, and the one measured platform difference in the whole module.

**Files:**
- Create: `channel/endpoint.go`, `channel/reader_unix.go`, `channel/reader_windows.go`
- Create: `channel/endpoint_test.go`
- Modify: `channel/errors.go`

**Interfaces:**
- Produces, exported: `channel.Discover`, `channel.Endpoint`, `channel.EndpointKind` and its three values, `channel.FDVar`, `channel.PipeVar`, `channel.VersionVar`, `channel.ErrUnusable`.
- Produces, package-internal: `discover`, `connection`, `openDescriptor`, `openPipe`, `nameVar`, `firstInheritableFD`.

The three variable names are exported because the Rust crate exports its three, and an app that reads `SHEP_CHANNEL_FD` itself should not have to spell it. `SHEP_NAME` stays unexported, as it does there: it is how this package recognises shep, not part of the channel.

- [ ] **Step 1: Write the failing discovery tests**

`channel/endpoint_test.go`:

```go
package channel

import (
	"errors"
	"strings"
	"testing"
)

// fakeEnv is a lookup over a map. No test mutates the process
// environment, which would race every other test.
func fakeEnv(pairs map[string]string) lookup {
	return func(name string) (string, bool) {
		value, set := pairs[name]
		return value, set
	}
}

func TestNeitherVariableMeansNoChannel(t *testing.T) {
	found, err := discover(fakeEnv(map[string]string{nameVar: "web"}))
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if found.Kind != EndpointAbsent {
		t.Fatalf("kind is %v, want absent", found.Kind)
	}
}

func TestADescriptorIsTakenFromTheEnvironment(t *testing.T) {
	found, err := discover(fakeEnv(map[string]string{FDVar: " 3 "}))
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if found.Kind != EndpointDescriptor || found.FD != 3 {
		t.Fatalf("found %+v, want descriptor 3", found)
	}
}

func TestAPipePathIsTakenFromTheEnvironment(t *testing.T) {
	path := `\\.\pipe\shep-channel-1234-0-0123456789abcdef`
	found, err := discover(fakeEnv(map[string]string{PipeVar: path}))
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if found.Kind != EndpointPipe || found.Pipe != path {
		t.Fatalf("found %+v, want pipe %s", found, path)
	}
}

// Taking 1 would give this module the app's stdout: it would write JSON
// into it and close it on exit. Worse than a merely wrong number.
func TestADescriptorBelowThreeIsRefused(t *testing.T) {
	for _, raw := range []string{"0", "1", "2", "-1"} {
		_, err := discover(fakeEnv(map[string]string{FDVar: raw}))
		if !errors.Is(err, ErrUnusable) {
			t.Fatalf("%s=%s reported %v, want ErrUnusable", FDVar, raw, err)
		}
		if !strings.Contains(err.Error(), FDVar) || !strings.Contains(err.Error(), raw) {
			t.Fatalf("the refusal names neither the variable nor the value: %v", err)
		}
	}
}

func TestADescriptorThatIsNotANumberIsRefused(t *testing.T) {
	_, err := discover(fakeEnv(map[string]string{FDVar: "three"}))
	if !errors.Is(err, ErrUnusable) {
		t.Fatalf("%s=three reported %v, want ErrUnusable", FDVar, err)
	}
	if !strings.Contains(err.Error(), "three") {
		t.Fatalf("the refusal does not name the value: %v", err)
	}
}

func TestAnEmptyPipePathIsRefusedRatherThanOpened(t *testing.T) {
	if _, err := discover(fakeEnv(map[string]string{PipeVar: "   "})); !errors.Is(err, ErrUnusable) {
		t.Fatalf("%s= reported %v, want ErrUnusable", PipeVar, err)
	}
}

// The shepherd sets exactly one. This pins what happens if that stops
// being true, rather than leaving it to chance.
func TestTheDescriptorWinsWhenBothVariablesAreSet(t *testing.T) {
	found, err := discover(fakeEnv(map[string]string{FDVar: "3", PipeVar: `\\.\pipe\x`}))
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if found.Kind != EndpointDescriptor {
		t.Fatalf("kind is %v, want descriptor", found.Kind)
	}
}

// The exported wrapper reads the process environment, unlike every test
// above. It only looks, so nothing here opens or claims a channel.
func TestDiscoverReadsTheProcessEnvironment(t *testing.T) {
	t.Setenv(FDVar, " 3 ")
	found, err := Discover()
	if err != nil {
		t.Fatalf("Discover: %v", err)
	}
	if found.Kind != EndpointDescriptor || found.FD != 3 {
		t.Fatalf("Discover found %+v, want descriptor 3", found)
	}
}

func TestEveryEndpointKindPrintsItsName(t *testing.T) {
	names := map[EndpointKind]string{
		EndpointAbsent:     "absent",
		EndpointDescriptor: "descriptor",
		EndpointPipe:       "pipe",
	}
	for kind, want := range names {
		if got := kind.String(); got != want {
			t.Fatalf("kind %d prints %q, want %q", int(kind), got, want)
		}
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test ./...`
Expected: FAIL to build, `discover` and the rest are undefined.

- [ ] **Step 3: Write discovery**

First append to `channel/errors.go`, so every refusal below is one an app can classify rather than one it has to read:

```go
// ErrUnusable means the environment names a channel that cannot be
// opened here.
//
// A broken environment rather than an absent one, so it is loud. Set
// SHEP_CHANNEL_PIPE on unix and this is what comes back.
var ErrUnusable = errors.New("unusable shepherd channel")
```

Then `channel/endpoint.go`:

```go
package channel

import (
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
)

const (
	// FDVar names the inherited descriptor. Set on unix only.
	FDVar = "SHEP_CHANNEL_FD"
	// PipeVar names the pipe path. Set on Windows only.
	PipeVar = "SHEP_CHANNEL_PIPE"
	// VersionVar carries the wire stamp whenever a channel exists.
	VersionVar = "SHEP_CHANNEL_VERSION"
	// nameVar is injected by shep and cannot be set by hand. So it
	// answers whether this process runs under shep.
	nameVar = "SHEP_NAME"
)

// firstInheritableFD is the lowest number the channel can arrive on. The
// app's own standard streams are 0, 1 and 2.
const firstInheritableFD = 3

// EndpointKind says which door a channel is behind, if there is one.
type EndpointKind int

const (
	// EndpointAbsent means the operator opened no channel here.
	EndpointAbsent EndpointKind = iota
	// EndpointDescriptor means an inherited descriptor, from FDVar.
	EndpointDescriptor
	// EndpointPipe means a named pipe path, from PipeVar.
	EndpointPipe
)

// String names the kind. A refusal and a failed assertion then read as
// words, not as a number.
func (k EndpointKind) String() string {
	switch k {
	case EndpointAbsent:
		return "absent"
	case EndpointDescriptor:
		return "descriptor"
	case EndpointPipe:
		return "pipe"
	default:
		return fmt.Sprintf("EndpointKind(%d)", int(k))
	}
}

// Endpoint is where this process's channel is, if it has one.
type Endpoint struct {
	// Kind says which of the two fields below carries meaning.
	Kind EndpointKind
	// FD is the inherited descriptor, set when Kind is
	// EndpointDescriptor.
	FD int
	// Pipe is the named pipe path, set when Kind is EndpointPipe.
	Pipe string
}

// lookup reads one environment variable. os.LookupEnv in production, a
// map in tests, so no test mutates the process environment.
type lookup func(string) (string, bool)

// connection is an open channel. Every half is the same object on unix;
// on Windows the reader wraps it.
type connection struct {
	reader io.Reader
	writer io.Writer
	handle io.Closer
}

// Discover reads the environment and says where the channel is.
//
// Branches on which variable is present, never on the platform: the
// shepherd sets exactly one of them, and neither is the ordinary case.
func Discover() (Endpoint, error) {
	return discover(os.LookupEnv)
}

// discover is Discover with its environment injected.
func discover(get lookup) (Endpoint, error) {
	if raw, set := get(FDVar); set {
		fd, err := strconv.Atoi(strings.TrimSpace(raw))
		if err != nil {
			return Endpoint{}, fmt.Errorf("%w: %s=%s is not a descriptor", ErrUnusable, FDVar, raw)
		}
		if fd < firstInheritableFD {
			return Endpoint{}, fmt.Errorf(
				"%w: %s=%d must be %d or above, since 0, 1 and 2 are this process's own standard streams",
				ErrUnusable, FDVar, fd, firstInheritableFD)
		}
		return Endpoint{Kind: EndpointDescriptor, FD: fd}, nil
	}
	if raw, set := get(PipeVar); set {
		if strings.TrimSpace(raw) == "" {
			return Endpoint{}, fmt.Errorf("%w: %s is set and empty", ErrUnusable, PipeVar)
		}
		return Endpoint{Kind: EndpointPipe, Pipe: raw}, nil
	}
	return Endpoint{Kind: EndpointAbsent}, nil
}
```

The ownership guard is not here. It belongs beside the take rather than beside the lookup, and Task 6 adds a second door onto the same descriptor, so it lives in `openConn` and both doors go through it. `Discover` claims nothing: it reads the environment and opens no handle.

- [ ] **Step 4: Write the unix read half**

`channel/reader_unix.go`:

```go
//go:build !windows

package channel

import (
	"fmt"
	"net"
	"os"
)

// openDescriptor takes the inherited socket and returns the open channel.
//
// Reads go straight to the conn. A socketpair's two ends are independent
// open file descriptions. A parked read costs a concurrent write
// nothing. net.FileConn hands the descriptor to the runtime poller.
func openDescriptor(fd int) (*connection, error) {
	file := os.NewFile(uintptr(fd), "shep-channel")
	if file == nil {
		return nil, fmt.Errorf("%w: %s=%d is not an open descriptor", ErrUnusable, FDVar, fd)
	}
	conn, err := net.FileConn(file)
	// FileConn duplicates the descriptor, so this closes only our copy.
	// Leaving it open would leak the number for the process's lifetime.
	closeErr := file.Close()
	if err != nil {
		return nil, fmt.Errorf(
			"%w: %s=%d is not an open socket (%v): the shepherd passes the channel as one end of a socketpair",
			ErrUnusable, FDVar, fd, err)
	}
	if closeErr != nil {
		return nil, fmt.Errorf("%w: %s=%d could not be handed over: %v", ErrUnusable, FDVar, fd, closeErr)
	}
	return &connection{reader: conn, writer: conn, handle: conn}, nil
}

// openPipe refuses a Windows named pipe on a platform that has none.
func openPipe(path string) (*connection, error) {
	return nil, fmt.Errorf(
		"%w: %s=%s names a Windows named pipe and this is not Windows", ErrUnusable, PipeVar, path)
}
```

- [ ] **Step 5: Write the Windows read half**

This is the measured one. Three probes ran on real Windows against a real shepherd on 2026-09-06, each with a reader parked and a writer trying to send:

| approach | result |
|---|---|
| `os.OpenFile`, reader and writer goroutines | write blocked, 6s |
| `FILE_FLAG_OVERLAPPED` through `syscall.CreateFile`, wrapped in `os.NewFile` | write blocked, 6s |
| `PeekNamedPipe` poll loop | write completed, 0s |

The shepherd hands a Windows app one pipe instance, and Windows serialises every operation on a synchronous file object, so a reader parked in `ReadFile` holds it against the writer. The fix the Rust crate names as the proper one does not port: Go issues a synchronous read whatever flags the handle was opened with, so wrapping an overlapped handle in `os.NewFile` changes nothing. Read `crates/shep-channel/src/endpoint.rs`'s `PipeReader` for the equivalent Rust reasoning; this is the same shape reached a different way.

`channel/reader_windows.go`:

```go
//go:build windows

package channel

import (
	"fmt"
	"io"
	"os"
	"syscall"
	"time"
	"unsafe"
)

// pipePollInterval is how long the reader waits between peeks at an empty
// pipe. Invisible next to action_timeout, which is seconds.
const pipePollInterval = 20 * time.Millisecond

// errPipeNotConnected is ERROR_PIPE_NOT_CONNECTED, which package syscall
// does not name. With ERROR_BROKEN_PIPE it is this channel's end of
// stream.
const errPipeNotConnected = syscall.Errno(233)

var (
	kernel32          = syscall.NewLazyDLL("kernel32.dll")
	procPeekNamedPipe = kernel32.NewProc("PeekNamedPipe")
)

// pipeReader reads the channel's pipe without parking inside ReadFile.
//
// The shepherd hands this process one pipe instance and the writer holds
// the same object. A parked read would hold it against every write.
type pipeReader struct {
	pipe *os.File
}

// buffered reports how many bytes are waiting. open is false once the
// shepherd has closed its end and every sent byte is drained.
func (r *pipeReader) buffered() (waiting uint32, open bool, err error) {
	var available uint32
	// LazyProc.Call carries //go:uintptrescapes. A pointer converted to
	// uintptr in this argument list stays alive and unmoved. That is
	// what makes &available sound here.
	reported, _, callErr := procPeekNamedPipe.Call(
		r.pipe.Fd(),
		0,
		0,
		0,
		uintptr(unsafe.Pointer(&available)),
		0,
	)
	if reported == 0 {
		errno, isErrno := callErr.(syscall.Errno)
		if isErrno && (errno == syscall.ERROR_BROKEN_PIPE || errno == errPipeNotConnected) {
			return 0, false, nil
		}
		return 0, false, fmt.Errorf("peek the shepherd channel: %w", callErr)
	}
	return available, true, nil
}

// Read fills p with whatever the pipe already holds, waiting by polling.
func (r *pipeReader) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	for {
		waiting, open, err := r.buffered()
		if err != nil {
			return 0, err
		}
		if !open {
			return 0, io.EOF
		}
		if waiting == 0 {
			time.Sleep(pipePollInterval)
			continue
		}
		// Never asks for more than the peek reported. A larger read
		// parks in the kernel again and revives the deadlock.
		want := len(p)
		if int(waiting) < want {
			want = int(waiting)
		}
		return r.pipe.Read(p[:want])
	}
}

// openPipe opens the named pipe the shepherd created for this process.
func openPipe(path string) (*connection, error) {
	pipe, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		return nil, fmt.Errorf("%w: %s=%s could not be opened: %v", ErrUnusable, PipeVar, path, err)
	}
	return &connection{reader: &pipeReader{pipe: pipe}, writer: pipe, handle: pipe}, nil
}

// openDescriptor refuses an inherited descriptor on a platform that does
// not inherit one.
func openDescriptor(fd int) (*connection, error) {
	return nil, fmt.Errorf(
		"%w: %s=%d names an inherited descriptor and Windows does not inherit one", ErrUnusable, FDVar, fd)
}
```

- [ ] **Step 6: Run to verify it passes, and that the other platform compiles**

Run: `go test ./...`
Expected: PASS.

Then compile the arm this host never runs, one command at a time. From a unix host:

```bash
GOOS=windows GOARCH=amd64 go build ./...
```
```bash
GOOS=windows GOARCH=amd64 go vet ./...
```
Expected: EXIT=0 for both.

From a Windows host, the mirror image with `GOOS=linux`. This is compile coverage and nothing more: a `cfg`-style arm that compiles has been checked for spelling, not for behaviour, and shep already shipped one bug that proves the difference. Task 10 and Task 12 are what actually run this file.

- [ ] **Step 7: Prove the descriptor floor is not vacuous**

Delete the `fd < firstInheritableFD` branch in `discover`. Run `go test ./...` and confirm `TestADescriptorBelowThreeIsRefused` fails on `0`. Restore.

- [ ] **Step 8: Commit**

```bash
git add channel/endpoint.go channel/reader_unix.go channel/reader_windows.go channel/endpoint_test.go
git commit -m "feat(channel): find the channel and open it on both platforms"
```

---

## Task 6: The low layer an app can drive itself

**Repo: `shep-pm/shep-go`.**

D1 asks for two layers over one code path: a low-level channel with `open`, `recv` and `send` and no threads, and a handler layer built on top of it. Everything above this point is unexported, so an app that already runs its own event loop cannot reach the channel at all. This task is that low layer, and Task 9's `start` opens through the same function rather than repeating it.

Three shapes here differ from the Rust crate's, and each is a Go idiom rather than a change of contract.

**Absence is a sentinel error, not a nil handle.** Rust returns `Ok(None)`. Go has no `Option`, and `(nil, nil)` would leave a nil dereference one forgotten check away. `ErrNoChannel` is checked with `errors.Is`, the way `io.EOF` and `os.ErrNotExist` are.

**`Recv` and `Send` pass values, not pointers.** Rust's `&ChildMessage` avoids a move it cares about; a `ChildMessage` here is six words, and a pointer parameter would add a nil case to every call. The same reasoning exported `NewReady`, `NewMetric` and `NewReply` back in Task 3: `ptr` stays unexported, so without them an app on this layer could not build a metric at all.

**The ownership guard arrives with this task.** Task 5 carries none, because `Serve` was the only door and it is a `sync.Once`. `Open` is a second door onto the same descriptor, so the claim lives in `openConn`, which both doors go through. A second `Open`, or an `Open` after `Serve`, is `ErrAlreadyTaken`. A refusal releases the claim, or one bad value would refuse every later call in the process.

**Files:**
- Create: `channel/conn.go`, `channel/conn_test.go`, `channel/conn_unix_test.go`
- Modify: `channel/errors.go`

**Interfaces:**
- Consumes: `discover`, `connection`, `openDescriptor` and `openPipe` from Task 5; `readMessage` and `writeMessage` from Task 3.
- Produces, exported: `channel.Open`, `channel.Conn`, `(*Conn).Recv`, `(*Conn).Send`, `(*Conn).Version`, `(*Conn).Close`, `channel.ErrNoChannel`, `channel.ErrAlreadyTaken`.
- Produces, package-internal: `openConn`, `channelTaken`, `(*Conn).intoHalves`, and the two test helpers `fakeShepherd` and `releaseChannel` that Task 9 also uses.

- [ ] **Step 1: Write the failing tests**

`channel/conn_test.go`:

```go
package channel

import (
	"errors"
	"testing"
)

func TestOpenReportsNoChannelWhenNeitherVariableIsSet(t *testing.T) {
	conn, err := openConn(fakeEnv(map[string]string{nameVar: "web"}))
	if !errors.Is(err, ErrNoChannel) {
		t.Fatalf("openConn returned %v, want ErrNoChannel", err)
	}
	if conn != nil {
		t.Fatalf("openConn refused and handed back %+v", conn)
	}
	if channelTaken.Load() {
		t.Fatal("an absent channel was claimed")
	}
}

// The exported door reads the process environment. A descriptor discover
// refuses opens nothing, so this claims nothing either.
func TestOpenReadsTheProcessEnvironment(t *testing.T) {
	t.Setenv(FDVar, "1")
	if _, err := Open(); !errors.Is(err, ErrUnusable) {
		t.Fatalf("Open returned %v, want ErrUnusable", err)
	}
	if channelTaken.Load() {
		t.Fatal("a refused descriptor claimed the channel")
	}
}
```

`channel/conn_unix_test.go`. `fakeShepherd` lives here rather than beside `Serve`, because this is the first task that needs a real descriptor and Task 9 needs the same one:

```go
//go:build !windows

package channel

import (
	"bufio"
	"errors"
	"fmt"
	"net"
	"os"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"
)

// connDeadline bounds every wait on a real socketpair below.
const connDeadline = 5 * time.Second

// releaseChannel drops this process's claim on the channel.
//
// The claim is process-global and Go runs a package's tests in one
// process. Production never releases it: shep opens one channel per
// process and never a second.
func releaseChannel() { channelTaken.Store(false) }

// fakeShepherd hands back a real socketpair: one end's descriptor number
// for the library, the other end wrapped for the test to drive.
func fakeShepherd(t *testing.T) (appFD int, shepherd net.Conn) {
	t.Helper()
	pair, err := syscall.Socketpair(syscall.AF_UNIX, syscall.SOCK_STREAM, 0)
	if err != nil {
		t.Fatalf("socketpair: %v", err)
	}
	ours := os.NewFile(uintptr(pair[0]), "shepherd-end")
	conn, err := net.FileConn(ours)
	if err != nil {
		t.Fatalf("wrap the shepherd's end: %v", err)
	}
	if err := ours.Close(); err != nil {
		t.Fatalf("close our duplicate: %v", err)
	}
	if err := conn.SetDeadline(time.Now().Add(connDeadline)); err != nil {
		t.Fatalf("set the deadline: %v", err)
	}
	t.Cleanup(func() {
		conn.Close()
		releaseChannel()
	})
	return pair[1], conn
}

// D1's reason for existing. An app that owns its event loop drives the
// channel from inside it. Nothing of ours runs beside it.
func TestAnAppDrivesRecvAndSendWithNoGoroutines(t *testing.T) {
	appFD, shepherd := fakeShepherd(t)
	fromApp := bufio.NewReader(shepherd)
	// A collection first. The runtime's own workers then exist before
	// the baseline, and cannot be counted as ours.
	runtime.GC()
	before := runtime.NumGoroutine()

	conn, err := openConn(fakeEnv(map[string]string{
		FDVar:      fmt.Sprint(appFD),
		VersionVar: Version,
	}))
	if err != nil {
		t.Fatalf("openConn: %v", err)
	}
	defer conn.Close()
	if conn.Version() != Version {
		t.Fatalf("Version is %q, want %q", conn.Version(), Version)
	}

	// Closing the shepherd's end ends a parked Recv. A regression fails
	// this test rather than hanging the suite.
	stop := time.AfterFunc(connDeadline, func() { shepherd.Close() })

	if err := conn.Send(NewReady()); err != nil {
		t.Fatalf("Send readiness: %v", err)
	}
	line, err := fromApp.ReadString('\n')
	if err != nil {
		t.Fatalf("the shepherd never received readiness: %v", err)
	}
	if strings.TrimRight(line, "\r\n") != `{"kind":"ready"}` {
		t.Fatalf("the shepherd received %q", line)
	}

	if _, err := shepherd.Write([]byte("{\"kind\":\"action\",\"name\":\"gc\",\"id\":7}\n")); err != nil {
		t.Fatalf("send the action: %v", err)
	}
	message, err := conn.Recv()
	if err != nil {
		t.Fatalf("Recv: %v", err)
	}
	if message.Kind != KindAction || *message.Name != "gc" || *message.ID != 7 {
		t.Fatalf("Recv returned %+v", message)
	}
	if err := conn.Send(NewReply(*message.Name, "collected", message.ID)); err != nil {
		t.Fatalf("Send the reply: %v", err)
	}
	reply, err := fromApp.ReadString('\n')
	if err != nil {
		t.Fatalf("the shepherd never received the reply: %v", err)
	}
	if !strings.Contains(reply, `"id":7`) {
		t.Fatalf("the reply is %q", reply)
	}

	stop.Stop()
	if after := runtime.NumGoroutine(); after > before {
		t.Fatalf("the low layer left %d goroutine(s) running", after-before)
	}
}

// D7's guard, at the door Serve opens through as well.
func TestASecondOpenIsRefused(t *testing.T) {
	appFD, _ := fakeShepherd(t)
	env := fakeEnv(map[string]string{FDVar: fmt.Sprint(appFD)})

	conn, err := openConn(env)
	if err != nil {
		t.Fatalf("the first open: %v", err)
	}
	defer conn.Close()

	if _, err := openConn(env); !errors.Is(err, ErrAlreadyTaken) {
		t.Fatalf("the second open returned %v, want ErrAlreadyTaken", err)
	}
}

func TestRecvAndSendRefuseAfterClose(t *testing.T) {
	appFD, _ := fakeShepherd(t)
	conn, err := openConn(fakeEnv(map[string]string{FDVar: fmt.Sprint(appFD)}))
	if err != nil {
		t.Fatalf("openConn: %v", err)
	}
	if err := conn.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := conn.Close(); err != nil {
		t.Fatalf("a second Close returned %v, want nil", err)
	}
	if _, err := conn.Recv(); !errors.Is(err, ErrClosed) {
		t.Fatalf("Recv after Close returned %v, want ErrClosed", err)
	}
	if err := conn.Send(NewReady()); !errors.Is(err, ErrClosed) {
		t.Fatalf("Send after Close returned %v, want ErrClosed", err)
	}
}

// An open that failed claimed nothing. Keeping the claim would refuse
// every later call in this process over one bad value.
func TestAFailedOpenReleasesTheClaim(t *testing.T) {
	// openDescriptor closes the number it was handed, so this file needs
	// no Close of its own.
	file, err := os.CreateTemp(t.TempDir(), "not-a-socket")
	if err != nil {
		t.Fatalf("temp file: %v", err)
	}

	_, err = openConn(fakeEnv(map[string]string{FDVar: fmt.Sprint(file.Fd())}))
	if !errors.Is(err, ErrUnusable) {
		t.Fatalf("a regular file opened as %v, want ErrUnusable", err)
	}
	if channelTaken.Load() {
		t.Fatal("a failed open kept the claim")
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test -race ./...`
Expected: FAIL to build, `openConn`, `Conn` and the two errors are undefined.

- [ ] **Step 3: Add the two errors the low layer needs**

Append to `channel/errors.go`:

```go
// ErrNoChannel means the operator opened no channel for this process.
//
// The ordinary case rather than a failure. Serve answers it with a
// handle whose every method does nothing.
var ErrNoChannel = errors.New("no shepherd channel on this process")

// ErrAlreadyTaken means this process already took its channel.
//
// One descriptor, one owner. Serve takes it through the same door, so a
// call after Serve meets this too.
var ErrAlreadyTaken = errors.New("the shepherd channel has already been taken by this process")
```

- [ ] **Step 4: Write the low layer**

`channel/conn.go`:

```go
package channel

import (
	"bufio"
	"io"
	"os"
	"sync/atomic"
)

// channelTaken guards the inherited channel against a second take.
//
// Claimed only by a call that goes on to open something. A refusal
// leaves it alone, or one bad descriptor would refuse every later call.
var channelTaken atomic.Bool

// Conn is the shepherd channel with no goroutines: the caller owns the
// loop.
//
// Serve is the documented default, because it answers an action nobody
// registered. Reach for Conn when the app already runs an event loop of
// its own.
//
// Not safe for concurrent use. One goroutine drives Recv and Send.
type Conn struct {
	reader  *bufio.Reader
	writer  io.Writer
	handle  io.Closer
	version string
	closed  bool
}

// Open takes this process's channel and hands back the low layer.
//
// ErrNoChannel means the operator opened none, which is ordinary rather
// than a failure. ErrAlreadyTaken means this process took its channel
// already, Serve included: one descriptor has one owner. ErrUnusable
// means the environment names a channel that cannot be opened here.
func Open() (*Conn, error) {
	return openConn(os.LookupEnv)
}

// openConn is Open with its environment injected, so a test needs no
// process-wide variable.
func openConn(get lookup) (*Conn, error) {
	found, err := discover(get)
	if err != nil {
		return nil, err
	}
	if found.Kind == EndpointAbsent {
		return nil, ErrNoChannel
	}
	if channelTaken.Swap(true) {
		return nil, ErrAlreadyTaken
	}
	var opened *connection
	if found.Kind == EndpointDescriptor {
		opened, err = openDescriptor(found.FD)
	} else {
		opened, err = openPipe(found.Pipe)
	}
	if err != nil {
		// Nothing was taken. Keeping the claim would refuse a later
		// call that might work.
		channelTaken.Store(false)
		return nil, err
	}
	version, _ := get(VersionVar)
	return &Conn{
		reader:  bufio.NewReader(opened.reader),
		writer:  opened.writer,
		handle:  opened.handle,
		version: version,
	}, nil
}

// Recv reads one message from the shepherd.
//
// io.EOF is the shepherd closing its end. A line that will not parse is
// ErrMalformed. The next call resumes at the line after it.
func (c *Conn) Recv() (ShepherdMessage, error) {
	if c.closed {
		return ShepherdMessage{}, ErrClosed
	}
	return readMessage(c.reader)
}

// Send writes one message and its newline.
//
// Blocks until the transport takes it. Nothing is queued here, so a slow
// shepherd is the caller's to handle.
func (c *Conn) Send(message ChildMessage) error {
	if c.closed {
		return ErrClosed
	}
	return writeMessage(c.writer, message)
}

// Version is the SHEP_CHANNEL_VERSION stamp, empty when there was none.
//
// A stamp, not a negotiation. An app can notice a wire it has never
// seen. It cannot ask for a different one.
func (c *Conn) Version() string { return c.version }

// Close releases the transport. A second call does nothing.
//
// Recv and Send return ErrClosed afterwards. The claim on the channel
// stays: shep opens one per process and never a second.
func (c *Conn) Close() error {
	if c.closed {
		return nil
	}
	c.closed = true
	return c.handle.Close()
}

// intoHalves takes the channel apart for the two goroutines that drive
// it. Serve is the only caller.
func (c *Conn) intoHalves() (*bufio.Reader, io.Writer, string) {
	return c.reader, c.writer, c.version
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `go test -race ./...`
Expected: PASS.

Then compile the arm this host never runs:

```bash
GOOS=windows GOARCH=amd64 go vet ./...
```
Expected: EXIT=0.

- [ ] **Step 6: Prove all four guards are not vacuous**

One mutation at a time, each restored before the next.

Delete `channelTaken.Store(false)` from `openConn`'s failure branch. Run `go test -race ./...` and confirm `TestAFailedOpenReleasesTheClaim` fails saying a failed open kept the claim. Restore.

Delete the whole `if channelTaken.Swap(true)` block. Confirm `TestASecondOpenIsRefused` fails, naming an `ErrUnusable` where it wanted `ErrAlreadyTaken`: with no guard the second call reaches a descriptor the first one already consumed. Restore.

Delete the `c.closed` check from `Recv`. Confirm `TestRecvAndSendRefuseAfterClose` fails on the Recv assertion rather than the Send one. Restore.

Add `go func() { select {} }()` as the last statement before `openConn` returns its `Conn`. Confirm `TestAnAppDrivesRecvAndSendWithNoGoroutines` fails saying the low layer left one goroutine running. That test is the whole of D1's "no threads" claim, and a counter that never moves would assert nothing. Restore.

- [ ] **Step 7: Commit**

```bash
git add channel/conn.go channel/conn_test.go channel/conn_unix_test.go channel/errors.go
git commit -m "feat(channel): open the channel without owning the loop"
```

---

## Task 7: The outbox and its two push policies

**Repo: `shep-pm/shep-go`.**

D4 falls out of the language here: a metric is a `select` with a `default` that counts a drop, and readiness and a reply are a blocking send raced against a closed channel.

**Files:**
- Create: `channel/outbox.go`, `channel/outbox_test.go`

**Interfaces:**
- Produces, package-internal: `outbox`, `newOutbox`, `pushLossy`, `pushBlocking`, `close`, `isClosed`, `droppedCount`, `drain`, `outboxCapacity`.

This differs from the Rust crate in one way worth knowing before comparing them. Rust evicts an already-queued metric to admit a readiness message sooner. Go makes readiness wait for the writer to drain one instead. The observable contract is the same and this is a third of the code.

- [ ] **Step 1: Write the failing tests**

`channel/outbox_test.go`:

```go
package channel

import (
	"errors"
	"strings"
	"testing"
	"time"
)

// outboxDeadline bounds every wait below. A working outbox answers in
// microseconds; this is slack for a loaded runner, not an expectation.
const outboxDeadline = 5 * time.Second

// failingWriter fails every write, so drain's give-up path needs no
// real transport.
type failingWriter struct{}

func (failingWriter) Write([]byte) (int, error) { return 0, errors.New("the shepherd went away") }

func TestAFullOutboxDropsAMetricAndCountsIt(t *testing.T) {
	out := newOutbox(1)
	out.pushLossy(NewMetric("rps", 1))
	out.pushLossy(NewMetric("rps", 2))
	out.pushLossy(NewMetric("rps", 3))

	if got := out.droppedCount(); got != 2 {
		t.Fatalf("dropped %d, want 2", got)
	}
	if message := <-out.messages; *message.Value != 1 {
		t.Fatalf("the queued sample is %v, want the first one", *message.Value)
	}
}

func TestALossyPushNeverBlocks(t *testing.T) {
	out := newOutbox(0)
	done := make(chan struct{})
	go func() {
		out.pushLossy(NewMetric("rps", 1))
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(outboxDeadline):
		t.Fatal("pushLossy parked on a full outbox")
	}
	if got := out.droppedCount(); got != 1 {
		t.Fatalf("dropped %d, want 1", got)
	}
}

func TestAMustDeliverPushWaitsForRoomAndThenProceeds(t *testing.T) {
	out := newOutbox(1)
	if err := out.pushBlocking(NewReady()); err != nil {
		t.Fatalf("the first message did not fit: %v", err)
	}

	done := make(chan error, 1)
	go func() { done <- out.pushBlocking(NewReady()) }()

	select {
	case err := <-done:
		t.Fatalf("pushBlocking returned %v while the outbox was full", err)
	case <-time.After(100 * time.Millisecond):
	}

	<-out.messages
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("pushBlocking after room: %v", err)
		}
	case <-time.After(outboxDeadline):
		t.Fatal("pushBlocking never proceeded after the outbox drained")
	}
}

// Without this, an app whose shepherd went away parks forever in Ready.
func TestClosingReleasesABlockedPushWithErrClosed(t *testing.T) {
	out := newOutbox(1)
	if err := out.pushBlocking(NewReady()); err != nil {
		t.Fatalf("the first message did not fit: %v", err)
	}

	done := make(chan error, 1)
	go func() { done <- out.pushBlocking(NewReady()) }()

	select {
	case err := <-done:
		t.Fatalf("pushBlocking returned %v too early", err)
	case <-time.After(100 * time.Millisecond):
	}

	out.close()
	select {
	case err := <-done:
		if !errors.Is(err, ErrClosed) {
			t.Fatalf("pushBlocking returned %v, want ErrClosed", err)
		}
	case <-time.After(outboxDeadline):
		t.Fatal("pushBlocking stayed parked after close")
	}
}

// Emitting a metric after the shepherd leaves is ordinary, not an error.
// The sample is gone, and droppedCount is how an app sees that.
func TestALossyPushAfterCloseCountsTheDrop(t *testing.T) {
	out := newOutbox(4)
	out.close()
	out.pushLossy(NewMetric("rps", 1))
	if got := out.droppedCount(); got != 1 {
		t.Fatalf("dropped %d, want 1", got)
	}
}

func TestAMustDeliverPushOnAClosedOutboxRefuses(t *testing.T) {
	out := newOutbox(4)
	out.close()
	if err := out.pushBlocking(NewReady()); !errors.Is(err, ErrClosed) {
		t.Fatalf("pushBlocking returned %v, want ErrClosed", err)
	}
}

// A reply queued just before the shepherd leaves still has to reach the
// wire. close is not a discard.
func TestDrainWritesWhatIsAlreadyQueuedAfterClose(t *testing.T) {
	out := newOutbox(4)
	if err := out.pushBlocking(NewReady()); err != nil {
		t.Fatalf("queue readiness: %v", err)
	}
	if err := out.pushBlocking(NewMetric("rps", 1)); err != nil {
		t.Fatalf("queue the metric: %v", err)
	}
	out.close()

	var written strings.Builder
	done := make(chan struct{})
	go func() {
		out.drain(&written)
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(outboxDeadline):
		t.Fatal("drain never returned on a closed outbox")
	}

	lines := strings.Split(strings.TrimSuffix(written.String(), "\n"), "\n")
	if len(lines) != 2 {
		t.Fatalf("drain wrote %d lines, want 2: %q", len(lines), written.String())
	}
	if !strings.Contains(lines[0], `"kind":"ready"`) || !strings.Contains(lines[1], `"kind":"metric"`) {
		t.Fatalf("drain wrote %q", written.String())
	}
}

// A failed write means the transport is gone. Closing there is what stops
// Ready reporting success for a message nothing will ever send.
func TestDrainClosesTheOutboxWhenAWriteFails(t *testing.T) {
	out := newOutbox(4)
	if err := out.pushBlocking(NewReady()); err != nil {
		t.Fatalf("queue readiness: %v", err)
	}

	done := make(chan struct{})
	go func() {
		out.drain(failingWriter{})
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(outboxDeadline):
		t.Fatal("drain never returned after a failed write")
	}
	if !out.isClosed() {
		t.Fatal("drain returned without closing the outbox")
	}
	if err := out.pushBlocking(NewReady()); !errors.Is(err, ErrClosed) {
		t.Fatalf("pushBlocking returned %v after a dead transport, want ErrClosed", err)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test ./...`
Expected: FAIL to build, `newOutbox` and the rest are undefined.

- [ ] **Step 3: Write the outbox**

`channel/outbox.go`:

```go
package channel

import (
	"io"
	"sync"
	"sync/atomic"
)

// outboxCapacity is how many messages may wait for the writer.
//
// ChildMessage is a handful of pointers. A full queue costs tens of
// kilobytes, plus what the names and bodies hold.
const outboxCapacity = 1024

// outbox is the queue between the app's goroutines and the one goroutine
// that writes.
//
// A dropped metric costs nothing: the shepherd logs metrics at debug
// level and reads them nowhere else. A dropped readiness hangs
// wait_ready, and a dropped reply costs an operator a whole
// action_timeout.
type outbox struct {
	messages  chan ChildMessage
	closed    chan struct{}
	closeOnce sync.Once
	dropped   atomic.Uint64
}

func newOutbox(capacity int) *outbox {
	return &outbox{
		messages: make(chan ChildMessage, capacity),
		closed:   make(chan struct{}),
	}
}

// pushLossy queues a message that may be dropped. Never blocks.
func (o *outbox) pushLossy(message ChildMessage) {
	if o.isClosed() {
		o.dropped.Add(1)
		return
	}
	select {
	case o.messages <- message:
	default:
		o.dropped.Add(1)
	}
}

// pushBlocking queues a message that must not be lost, waiting for room.
//
// Returns ErrClosed once the writer has stopped, rather than parking on
// a queue nothing drains.
func (o *outbox) pushBlocking(message ChildMessage) error {
	if o.isClosed() {
		return ErrClosed
	}
	select {
	case o.messages <- message:
		return nil
	case <-o.closed:
		return ErrClosed
	}
}

// close releases every waiter. Safe to call more than once.
//
// The message channel itself is never closed. A send racing a close on
// it would panic, and a metric during shutdown is ordinary.
func (o *outbox) close() {
	o.closeOnce.Do(func() { close(o.closed) })
}

func (o *outbox) isClosed() bool {
	select {
	case <-o.closed:
		return true
	default:
		return false
	}
}

// droppedCount is how many messages pushLossy has discarded.
func (o *outbox) droppedCount() uint64 { return o.dropped.Load() }

// drain writes queued messages until the transport fails or the outbox
// closes. It then writes whatever is still queued and returns.
func (o *outbox) drain(writer io.Writer) {
	defer o.close()
	for {
		select {
		case message := <-o.messages:
			if err := writeMessage(writer, message); err != nil {
				return
			}
		case <-o.closed:
			for {
				select {
				case message := <-o.messages:
					if err := writeMessage(writer, message); err != nil {
						return
					}
				default:
					return
				}
			}
		}
	}
}
```

- [ ] **Step 4: Run to verify it passes, with the race detector**

Run: `go test -race ./...`
Expected: PASS, no race reported. Every test from here on runs with `-race`: this module is two goroutines around one shared handle, which is the shape the detector exists for.

- [ ] **Step 5: Prove the drop policy is not vacuous**

Change `pushLossy`'s `default` branch to fall through to a blocking send:

```go
	o.messages <- message
```

Confirm `TestALossyPushNeverBlocks` fails at its deadline and `TestAFullOutboxDropsAMetricAndCountsIt` fails or hangs to the same deadline. Restore.

Then delete `defer o.close()` from `drain` and confirm `TestDrainClosesTheOutboxWhenAWriteFails` fails. Restore.

- [ ] **Step 6: Commit**

```bash
git add channel/outbox.go channel/outbox_test.go
git commit -m "feat(channel): queue what must be sent and drop what may be"
```

---

## Task 8: The reply rule, as pure logic

**Repo: `shep-pm/shep-go`.**

The guarantee this module exists for. An app can forget to answer an action name it does not recognise; a library cannot.

**Files:**
- Create: `channel/dispatch.go`, `channel/dispatch_test.go`

**Interfaces:**
- Produces, package-internal: `dispatch`, `newDispatch`, `registerAction`, `registerShutdown`, `resolveAction`, `resolveShutdown`, `replyBody`, `runShutdown`.

Resolving a handler holds the registry's lock and running one does not. That ordering is not cosmetic: the Rust crate shipped it the other way first and a handler that registered a handler deadlocked the reader.

- [ ] **Step 1: Write the failing tests**

`channel/dispatch_test.go`:

```go
package channel

import (
	"strings"
	"testing"
	"time"
)

// dispatchDeadline bounds the deadlock test below. Without it a
// regression parks the whole suite instead of failing it.
const dispatchDeadline = 5 * time.Second

func TestARegisteredActionRunsItsHandler(t *testing.T) {
	registry := newDispatch()
	registry.registerAction("gc", func(a Action) string {
		return a.Name + " ran with " + strings.Join(a.Fields(), ",")
	})

	handler, registered := registry.resolveAction("gc")
	body := replyBody(handler, registered, Action{Name: "gc", Params: ptr("now please")})
	if body != "gc ran with now,please" {
		t.Fatalf("body is %q", body)
	}
}

// The contract calls this out: without a reply the operator waits out the
// whole action_timeout for a typo.
func TestAnUnregisteredActionStillGetsAReply(t *testing.T) {
	registry := newDispatch()
	handler, registered := registry.resolveAction("reload-config")
	body := replyBody(handler, registered, Action{Name: "reload-config"})
	if body != "unknown action: reload-config" {
		t.Fatalf("body is %q", body)
	}
}

// An app that panics should not cost the operator a timeout as well.
func TestAPanickingHandlerRepliesWithTheMessage(t *testing.T) {
	registry := newDispatch()
	registry.registerAction("boom", func(Action) string { panic("no such state") })

	handler, registered := registry.resolveAction("boom")
	body := replyBody(handler, registered, Action{Name: "boom"})
	if body != "action handler failed: no such state" {
		t.Fatalf("body is %q", body)
	}
}

func TestAShutdownHandlerRunsAndAPanicIsReported(t *testing.T) {
	registry := newDispatch()
	if _, registered := registry.resolveShutdown(); registered {
		t.Fatal("an empty registry reported a shutdown handler")
	}

	ran := false
	registry.registerShutdown(func() { ran = true })
	handler, registered := registry.resolveShutdown()
	if !registered {
		t.Fatal("the registered shutdown handler was not found")
	}
	if _, failed := runShutdown(handler); failed {
		t.Fatal("a working handler was reported as failed")
	}
	if !ran {
		t.Fatal("the shutdown handler never ran")
	}

	registry.registerShutdown(func() { panic("no such state") })
	handler, _ = registry.resolveShutdown()
	message, failed := runShutdown(handler)
	if !failed || message != "no such state" {
		t.Fatalf("a panicking handler reported %q, failed=%v", message, failed)
	}
}

// A reload action that swaps its own handlers is ordinary. Holding the
// registry's lock across the handler would deadlock on exactly that.
func TestAHandlerThatRegistersAHandlerDoesNotDeadlock(t *testing.T) {
	registry := newDispatch()
	registry.registerAction("reload", func(Action) string {
		registry.registerAction("late", func(Action) string { return "late ok" })
		return "reloaded"
	})

	done := make(chan string, 1)
	go func() {
		handler, registered := registry.resolveAction("reload")
		done <- replyBody(handler, registered, Action{Name: "reload"})
	}()

	select {
	case body := <-done:
		if body != "reloaded" {
			t.Fatalf("body is %q", body)
		}
	case <-time.After(dispatchDeadline):
		t.Fatal("a handler that registers a handler deadlocked the caller")
	}

	handler, registered := registry.resolveAction("late")
	if body := replyBody(handler, registered, Action{Name: "late"}); body != "late ok" {
		t.Fatalf("the handler registered inside a handler was not reachable: %q", body)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test -race ./...`
Expected: FAIL to build.

- [ ] **Step 3: Write the registry**

`channel/dispatch.go`:

```go
package channel

import (
	"fmt"
	"sync"
)

// dispatch holds the registered handlers.
//
// Resolving takes the lock and running does not. A handler that
// registers a handler would otherwise deadlock on the same lock.
type dispatch struct {
	mu       sync.RWMutex
	actions  map[string]func(Action) string
	shutdown func()
}

func newDispatch() *dispatch {
	return &dispatch{actions: make(map[string]func(Action) string)}
}

// registerAction replaces any handler already registered under name.
func (d *dispatch) registerAction(name string, handler func(Action) string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.actions[name] = handler
}

// registerShutdown replaces any handler already registered.
func (d *dispatch) registerShutdown(handler func()) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.shutdown = handler
}

// resolveAction returns the handler registered under name, if any.
func (d *dispatch) resolveAction(name string) (func(Action) string, bool) {
	d.mu.RLock()
	defer d.mu.RUnlock()
	handler, registered := d.actions[name]
	return handler, registered
}

// resolveShutdown returns the shutdown handler, if one is registered.
func (d *dispatch) resolveShutdown() (func(), bool) {
	d.mu.RLock()
	defer d.mu.RUnlock()
	return d.shutdown, d.shutdown != nil
}

// replyBody runs handler and returns what to send back.
//
// An unregistered name and a panicking handler both produce a body.
// Silence from either looks like an app thinking hard about a slow
// action.
func replyBody(handler func(Action) string, registered bool, action Action) (body string) {
	if !registered {
		return "unknown action: " + action.Name
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			body = fmt.Sprintf("action handler failed: %v", recovered)
		}
	}()
	return handler(action)
}

// runShutdown runs handler and reports whether it panicked.
//
// An unwind reaching the reader goroutine would take the process down.
// The app only wanted to stop gracefully.
func runShutdown(handler func()) (message string, failed bool) {
	defer func() {
		if recovered := recover(); recovered != nil {
			message = fmt.Sprint(recovered)
			failed = true
		}
	}()
	handler()
	return "", false
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `go test -race ./...`
Expected: PASS.

- [ ] **Step 5: Prove the reply rule is not vacuous**

Change `replyBody`'s unregistered branch to `return ""`. Confirm `TestAnUnregisteredActionStillGetsAReply` fails. Restore.

Then delete the `defer` and `recover` from `replyBody`. Confirm `TestAPanickingHandlerRepliesWithTheMessage` fails with a panic rather than an assertion, which is the same signal. Restore.

Then make `resolveAction` take the write lock and hold it across a call to the handler, by temporarily giving `dispatch` a `handle` method that does both under `d.mu.Lock()`, and point the deadlock test at it. Confirm `TestAHandlerThatRegistersAHandlerDoesNotDeadlock` fails at its deadline. Remove the temporary method.

- [ ] **Step 6: Commit**

```bash
git add channel/dispatch.go channel/dispatch_test.go
git commit -m "feat(channel): answer every action, including the ones nobody registered"
```

---

## Task 9: Serve, the two goroutines, and doing nothing well

**Repo: `shep-pm/shep-go`.**

D3, D6 and D7. The handle always exists, an app with no channel branches nowhere, and the warnings fire only where they mean something.

**Files:**
- Create: `channel/shepherd.go`, `channel/shepherd_test.go`, `channel/shepherd_unix_test.go`

**Interfaces:**
- Consumes: `openConn` and `(*Conn).intoHalves` from Task 6, the outbox from Task 7, the registry from Task 8, and `fakeShepherd` from Task 6's test file.
- Produces: `channel.Serve() *Shepherd`, `(*Shepherd).Ready() error`, `(*Shepherd).Metric(string, float64)`, `(*Shepherd).OnAction(string, func(Action) string) *Shepherd`, `(*Shepherd).OnShutdown(func()) *Shepherd`, `(*Shepherd).Active() bool`, `(*Shepherd).DroppedMetrics() uint64`, `(*Shepherd).Version() string`.

- [ ] **Step 1: Write the failing tests**

`channel/shepherd_test.go`:

```go
package channel

import (
	"bufio"
	"strings"
	"sync"
	"testing"
	"time"
)

// serveDeadline bounds the loops below. They run on their own goroutine
// and could otherwise park the suite.
const serveDeadline = 5 * time.Second

// collector gathers what the library warned about. A test asserts on the
// text and on how often it was said.
type collector struct {
	mu    sync.Mutex
	lines []string
}

func (c *collector) warn(message string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.lines = append(c.lines, message)
}

func (c *collector) said() []string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]string(nil), c.lines...)
}

// runReadLoop drives one canned stream through a shepherd and fails at
// the deadline rather than hanging.
func runReadLoop(t *testing.T, shepherd *Shepherd, stream string) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		shepherd.readLoop(bufio.NewReader(strings.NewReader(stream)))
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(serveDeadline):
		t.Fatal("readLoop never returned")
	}
}

func testShepherd(warn func(string)) *Shepherd {
	return &Shepherd{out: newOutbox(outboxCapacity), handlers: newDispatch(), warn: warn}
}

// D3: an app must be able to call every method without asking whether it
// has a channel.
func TestAnInertHandleAcceptsEverythingAndDoesNothing(t *testing.T) {
	shepherd := inert("", func(string) {})
	if shepherd.Active() {
		t.Fatal("an inert handle reported itself active")
	}
	shepherd.OnAction("gc", func(Action) string { return "ok" })
	shepherd.OnShutdown(func() {})
	shepherd.Metric("rps", 42)
	if err := shepherd.Ready(); err != nil {
		t.Fatalf("an inert Ready returned %v", err)
	}
	if got := shepherd.DroppedMetrics(); got != 0 {
		t.Fatalf("DroppedMetrics is %d, want 0", got)
	}
	if got := shepherd.Version(); got != "" {
		t.Fatalf("Version is %q, want empty", got)
	}
}

func TestAHandleStopsBeingActiveOnceTheChannelCloses(t *testing.T) {
	shepherd := testShepherd(func(string) {})
	if !shepherd.Active() {
		t.Fatal("a fresh channel did not read as live")
	}
	shepherd.out.close()
	if shepherd.Active() {
		t.Fatal("a handle whose shepherd went away still reads as live")
	}
}

// An author reading this line is deciding which field to set.
func TestTheNoChannelAdviceNamesEveryFieldThatOpensOne(t *testing.T) {
	for _, field := range []string{"channel = true", "wait_ready", "shutdown_with_message"} {
		if !strings.Contains(noChannelAdvice, field) {
			t.Fatalf("the advice does not mention %s", field)
		}
	}
}

// D5 makes this warning the only thing between a missing handler and a
// kill_timeout kill.
func TestTheUnhandledShutdownWarningNamesTheMethodToCall(t *testing.T) {
	for _, wanted := range []string{"OnShutdown", "kill_timeout"} {
		if !strings.Contains(unhandledShutdownAdvice, wanted) {
			t.Fatalf("the warning does not mention %s", wanted)
		}
	}
}

func TestTheNoChannelWarningFiresOnlyUnderShep(t *testing.T) {
	outside := &collector{}
	start(fakeEnv(map[string]string{}), outside.warn)
	if said := outside.said(); len(said) != 0 {
		t.Fatalf("an app running outside shep was warned: %v", said)
	}

	under := &collector{}
	start(fakeEnv(map[string]string{nameVar: "web"}), under.warn)
	said := under.said()
	if len(said) != 1 || !strings.Contains(said[0], "channel = true") {
		t.Fatalf("an app under shep with no channel was told %v", said)
	}
}

// D7: one descriptor has one owner. A second call hands back the first
// handle and says why. The only test here that calls the singleton,
// which is process-global and answers once.
func TestServeHandsBackOneHandleAndWarnsOnce(t *testing.T) {
	warnings := &collector{}
	env := fakeEnv(map[string]string{})

	first := serveShared(env, warnings.warn)
	second := serveShared(env, warnings.warn)
	third := serveShared(env, warnings.warn)

	if first != second || second != third {
		t.Fatalf("three calls handed back %p, %p and %p", first, second, third)
	}
	said := warnings.said()
	if len(said) != 1 {
		t.Fatalf("three calls produced %d warnings: %v", len(said), said)
	}
	for _, wanted := range []string{"more than once", "cannot be opened twice"} {
		if !strings.Contains(said[0], wanted) {
			t.Fatalf("the warning does not say %q: %s", wanted, said[0])
		}
	}
}

func TestTwoMalformedLinesWarnOnceAndTheLoopKeepsGoing(t *testing.T) {
	warnings := &collector{}
	shepherd := testShepherd(warnings.warn)
	stopped := make(chan struct{})
	shepherd.OnShutdown(func() { close(stopped) })

	runReadLoop(t, shepherd, "not json\nalso not json\n{\"kind\":\"shutdown\"}\n")

	select {
	case <-stopped:
	default:
		t.Fatal("the shutdown after two bad lines was never reached")
	}
	if said := warnings.said(); len(said) != 1 {
		t.Fatalf("two bad lines produced %d warnings: %v", len(said), said)
	}
}

// End of stream has to close the outbox. Otherwise the writer goroutine
// parks on a queue nobody will add to.
func TestEndOfStreamClosesTheOutbox(t *testing.T) {
	shepherd := testShepherd(func(string) {})
	runReadLoop(t, shepherd, "")
	if !shepherd.out.isClosed() {
		t.Fatal("readLoop returned without closing the outbox")
	}
}

func TestAnActionsReplyReachesTheOutboxCarryingItsID(t *testing.T) {
	shepherd := testShepherd(func(string) {})
	shepherd.OnAction("gc", func(a Action) string { return "collected " + strings.Join(a.Fields(), ",") })

	runReadLoop(t, shepherd, "{\"kind\":\"action\",\"name\":\"gc\",\"params\":\"now please\",\"id\":7}\n")

	reply := <-shepherd.out.messages
	if reply.Kind != KindActionReply || *reply.Action != "gc" {
		t.Fatalf("reply is %+v", reply)
	}
	if *reply.Body != "collected now,please" {
		t.Fatalf("body is %q", *reply.Body)
	}
	if reply.ID == nil || *reply.ID != 7 {
		t.Fatalf("the id was not echoed: %+v", reply.ID)
	}
}

func TestAnUnknownActionStillGetsAReplyCarryingItsID(t *testing.T) {
	shepherd := testShepherd(func(string) {})

	runReadLoop(t, shepherd, "{\"kind\":\"action\",\"name\":\"typo\",\"id\":8}\n")

	reply := <-shepherd.out.messages
	if *reply.Body != "unknown action: typo" {
		t.Fatalf("body is %q", *reply.Body)
	}
	if reply.ID == nil || *reply.ID != 8 {
		t.Fatalf("the id was not echoed: %+v", reply.ID)
	}
}

// Registering from inside a handler is what a reload action does. This
// runs the whole read loop, not just the registry. It covers the
// ordering the reader uses.
func TestAHandlerThatRegistersAHandlerDoesNotDeadlockTheReader(t *testing.T) {
	shepherd := testShepherd(func(string) {})
	shepherd.OnAction("reload", func(Action) string {
		shepherd.OnAction("late", func(Action) string { return "late ok" })
		return "reloaded"
	})

	runReadLoop(t, shepherd,
		"{\"kind\":\"action\",\"name\":\"reload\",\"id\":1}\n"+
			"{\"kind\":\"action\",\"name\":\"late\",\"id\":2}\n")

	first := <-shepherd.out.messages
	second := <-shepherd.out.messages
	if *first.Body != "reloaded" {
		t.Fatalf("the first body is %q", *first.Body)
	}
	if *second.Body != "late ok" {
		t.Fatalf("the second body is %q, so the late handler was not reachable", *second.Body)
	}
}
```

`channel/shepherd_unix_test.go`. `fakeShepherd` comes from Task 6's `conn_unix_test.go`, which is where the socketpair helper lives:

```go
//go:build !windows

package channel

import (
	"bufio"
	"fmt"
	"strings"
	"testing"
)

func TestStartOpensARealDescriptorAndSendsReadiness(t *testing.T) {
	appFD, shepherd := fakeShepherd(t)
	warnings := &collector{}

	handle := start(fakeEnv(map[string]string{
		FDVar:      fmt.Sprint(appFD),
		VersionVar: Version,
		nameVar:    "web",
	}), warnings.warn)

	if !handle.Active() {
		t.Fatal("a handle over a real descriptor is not active")
	}
	if handle.Version() != Version {
		t.Fatalf("Version is %q, want %q", handle.Version(), Version)
	}
	if err := handle.Ready(); err != nil {
		t.Fatalf("Ready: %v", err)
	}

	line, err := bufio.NewReader(shepherd).ReadString('\n')
	if err != nil {
		t.Fatalf("the shepherd never received readiness: %v", err)
	}
	if strings.TrimRight(line, "\r\n") != `{"kind":"ready"}` {
		t.Fatalf("the shepherd received %q", line)
	}
	if said := warnings.said(); len(said) != 0 {
		t.Fatalf("a working channel warned: %v", said)
	}
}

// D6: refusing would break every app on the day shep ships an additive 2.
func TestAnUnrecognisedVersionStampWarnsAndProceeds(t *testing.T) {
	appFD, shepherd := fakeShepherd(t)
	warnings := &collector{}

	handle := start(fakeEnv(map[string]string{
		FDVar:      fmt.Sprint(appFD),
		VersionVar: "99",
	}), warnings.warn)

	if err := handle.Ready(); err != nil {
		t.Fatalf("Ready after an unknown stamp: %v", err)
	}
	if _, err := bufio.NewReader(shepherd).ReadString('\n'); err != nil {
		t.Fatalf("the shepherd never received readiness: %v", err)
	}
	said := warnings.said()
	if len(said) != 1 || !strings.Contains(said[0], "99") {
		t.Fatalf("an unknown stamp produced %v", said)
	}
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `go test -race ./...`
Expected: FAIL to build.

- [ ] **Step 3: Write the handle**

`channel/shepherd.go`:

```go
package channel

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"sync"
	"sync/atomic"
)

// noChannelAdvice is what to tell an author running under shep with no
// channel.
const noChannelAdvice = "no channel on this process. Set `channel = true` " +
	"(or `wait_ready` / `shutdown_with_message`) on this app in the Flockfile to open one."

// unhandledShutdownAdvice is what to tell an author whose app was asked
// to stop and registered nothing.
const unhandledShutdownAdvice = "the shepherd sent shutdown and no OnShutdown handler is registered. " +
	"This process will be killed when kill_timeout expires. Register one to stop gracefully."

// Shepherd is a handle on this process's shepherd channel.
//
// Safe to use from any goroutine, and never nil. With no channel every
// method does nothing. An app needs no branch at its call sites.
type Shepherd struct {
	out      *outbox
	handlers *dispatch
	version  string
	warn     func(string)
}

var (
	serveOnce  sync.Once
	served     *Shepherd
	serveCalls atomic.Uint32
)

// Serve opens this process's channel and starts serving it.
//
// Always returns a usable handle. A second call returns the first one:
// the channel is one descriptor and cannot be owned twice.
func Serve() *Shepherd {
	return serveShared(os.LookupEnv, stderrWarn)
}

// serveShared is Serve with its environment and its warning sink
// injected. A test can call it twice and read what was said.
func serveShared(get lookup, warn func(string)) *Shepherd {
	serveOnce.Do(func() { served = start(get, warn) })
	if serveCalls.Add(1) == 2 {
		warn("Serve() called more than once; returning the first handle. " +
			"The channel is one descriptor and cannot be opened twice.")
	}
	return served
}

// stderrWarn writes one line where shep already collects it as bleats.
//
// The prefix is the same in every shep client library. An operator greps
// for one string whatever the app is written in.
func stderrWarn(message string) {
	fmt.Fprintln(os.Stderr, "shep-channel: "+message)
}

// inert is the handle an app gets with no channel.
func inert(version string, warn func(string)) *Shepherd {
	return &Shepherd{handlers: newDispatch(), version: version, warn: warn}
}

// start opens the channel through the low layer and spawns the two
// goroutines that drive it.
func start(get lookup, warn func(string)) *Shepherd {
	conn, err := openConn(get)
	if err != nil {
		if errors.Is(err, ErrNoChannel) {
			if _, underShep := get(nameVar); underShep {
				warn(noChannelAdvice)
			}
			return inert("", warn)
		}
		stamp, _ := get(VersionVar)
		warn(err.Error() + "; continuing without a channel")
		return inert(stamp, warn)
	}

	reader, writer, version := conn.intoHalves()
	if version != "" && version != Version {
		warn(fmt.Sprintf(
			"the shepherd stamps %s=%s and this module implements %s; continuing, since a newer wire has so far only added fields an older reader ignores",
			VersionVar, version, Version))
	}

	shepherd := &Shepherd{
		out:      newOutbox(outboxCapacity),
		handlers: newDispatch(),
		version:  version,
		warn:     warn,
	}
	go shepherd.out.drain(writer)
	go shepherd.readLoop(reader)
	return shepherd
}

// readLoop reads one message at a time and answers it.
//
// Handlers run on this goroutine, so a slow handler delays the next
// message. The shepherd's action_timeout is the budget for that.
func (s *Shepherd) readLoop(reader *bufio.Reader) {
	defer s.out.close()
	warnedMalformed := false
	for {
		message, err := readMessage(reader)
		if err != nil {
			if !errors.Is(err, ErrMalformed) {
				return
			}
			if !warnedMalformed {
				warnedMalformed = true
				s.warn(err.Error())
			}
			continue
		}
		if !s.answer(message) {
			return
		}
	}
}

// answer handles one message and reports whether the loop continues.
func (s *Shepherd) answer(message ShepherdMessage) bool {
	switch message.Kind {
	case KindShutdown:
		handler, registered := s.handlers.resolveShutdown()
		if !registered {
			s.warn(unhandledShutdownAdvice)
			return true
		}
		if panicked, failed := runShutdown(handler); failed {
			s.warn("shutdown handler panicked: " + panicked)
		}
		return true
	case KindAction:
		// decodeMessage refuses an action without both, so neither
		// dereference here can be nil.
		action := Action{Name: *message.Name, Params: message.Params}
		handler, registered := s.handlers.resolveAction(action.Name)
		body := replyBody(handler, registered, action)
		return s.out.pushBlocking(NewReply(action.Name, body, message.ID)) == nil
	default:
		// Unreachable: decodeMessage refuses every other kind.
		return true
	}
}

// Ready says this app is up. Blocks only until the message is queued.
//
// Returns ErrClosed when the shepherd has gone away. With no channel it
// returns nil: nothing to report, and no failure to handle.
func (s *Shepherd) Ready() error {
	if s.out == nil {
		return nil
	}
	return s.out.pushBlocking(NewReady())
}

// Metric records one sample. Never blocks and never fails.
//
// A sample may be dropped if the shepherd stops reading, which is what
// DroppedMetrics counts. That trade keeps a hot path off a full socket.
func (s *Shepherd) Metric(name string, value float64) {
	if s.out == nil {
		return
	}
	s.out.pushLossy(NewMetric(name, value))
}

// OnAction registers a handler for one action name, replacing any prior
// one. The returned string becomes the reply body.
//
// Safe to call from another goroutine, or from inside a handler: a
// reload action can swap its own handlers this way.
func (s *Shepherd) OnAction(name string, fn func(a Action) string) *Shepherd {
	s.handlers.registerAction(name, fn)
	return s
}

// OnShutdown registers the handler run when the shepherd asks this app
// to stop.
//
// Without one, a shutdown warns and nothing else happens. This package
// never ends a process on its own judgement.
func (s *Shepherd) OnShutdown(fn func()) *Shepherd {
	s.handlers.registerShutdown(fn)
	return s
}

// Active reports whether this process's channel is live right now.
//
// False before an operator opens one, and false again once the shepherd
// goes away. DroppedMetrics never freezes silently.
func (s *Shepherd) Active() bool {
	return s.out != nil && !s.out.isClosed()
}

// DroppedMetrics is how many samples were dropped because the shepherd
// was not keeping up. Always 0 without a channel.
func (s *Shepherd) DroppedMetrics() uint64 {
	if s.out == nil {
		return 0
	}
	return s.out.droppedCount()
}

// Version is the SHEP_CHANNEL_VERSION stamp, empty when the shepherd set
// none.
func (s *Shepherd) Version() string { return s.version }
```

- [ ] **Step 4: Run to verify it passes**

Run: `go test -race ./...`
Expected: PASS.

- [ ] **Step 5: Prove the inert path is not vacuous**

Change `Ready`'s `s.out == nil` branch to `return ErrClosed`. Confirm `TestAnInertHandleAcceptsEverythingAndDoesNothing` fails. Restore.

Then delete the `if _, underShep := get(nameVar); underShep` guard so the advice always fires. Confirm `TestTheNoChannelWarningFiresOnlyUnderShep` fails on its first half, which is the half that keeps an app running outside shep silent. Restore.

Then change `serveCalls.Add(1) == 2` to `>= 2`. Confirm `TestServeHandsBackOneHandleAndWarnsOnce` fails saying three calls produced three warnings. D7 asks for one warning naming the reason, not one per call, and `== 2` is the whole of that. Restore.

- [ ] **Step 6: Commit**

```bash
git add channel/shepherd.go channel/shepherd_test.go channel/shepherd_unix_test.go
git commit -m "feat(channel): serve the channel, and do nothing well without one"
```

---

## Task 10: One real child, on a real descriptor and a real pipe

**Repo: `shep-pm/shep-go`.**

Everything so far is covered without a second process. This is the piece none of it reaches: that `Serve` finds the channel in a process something else spawned, on both platforms, with the writer and reader running at the same time.

**Files:**
- Create: `channel/child_test.go`, `channel/harness_unix_test.go`, `channel/harness_windows_test.go`

**Interfaces:**
- Consumes: `Serve` from Task 9.

The re-exec pattern is the one `os/exec` uses for its own tests: the test binary runs itself with an environment guard, and `TestMain` branches on it before the suite starts. No second binary to build, and the child is exactly the code under test.

- [ ] **Step 1: Write the guard and the supervised app**

`channel/child_test.go`:

```go
package channel

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"reflect"
	"strings"
	"testing"
	"time"
)

// childGuard makes the test binary run as the supervised app instead of
// running the suite. os/exec's own tests re-exec themselves this way.
const childGuard = "SHEP_GO_CHANNEL_CHILD"

// childDeadline bounds every wait on the child. A working app answers in
// milliseconds; this is slack for a loaded runner.
const childDeadline = 20 * time.Second

func TestMain(m *testing.M) {
	if os.Getenv(childGuard) == "1" {
		runAsChild()
		return
	}
	os.Exit(m.Run())
}

// runAsChild is the supervised app the test below drives.
func runAsChild() {
	shepherd := Serve()
	shepherd.OnAction("gc", func(a Action) string {
		return "collected, fields=" + strings.Join(a.Fields(), ",")
	})
	shepherd.OnShutdown(func() { os.Exit(0) })
	if err := shepherd.Ready(); err != nil {
		fmt.Fprintln(os.Stderr, "ready:", err)
		os.Exit(1)
	}
	shepherd.Metric("rps", 42)
	// Parks the main goroutine. The reader goroutine does the work. A
	// pending timer keeps the runtime from calling this a deadlock.
	for {
		time.Sleep(time.Hour)
	}
}

// readLineWithin reads one line, or fails at the deadline. A hung child
// has to fail the test rather than park the suite.
func readLineWithin(t *testing.T, reader *bufio.Reader) string {
	t.Helper()
	type outcome struct {
		line string
		err  error
	}
	done := make(chan outcome, 1)
	go func() {
		line, err := reader.ReadString('\n')
		done <- outcome{line: line, err: err}
	}()
	select {
	case got := <-done:
		if got.err != nil {
			t.Fatalf("read from the child: %v", got.err)
		}
		return strings.TrimRight(got.line, "\r\n")
	case <-time.After(childDeadline):
		t.Fatalf("the child did not answer within %s", childDeadline)
		return ""
	}
}

// assertSameMessage compares two lines key by key. Hazard 1 rules out a
// byte comparison: Go writes 42 where the Rust fixtures write 42.0.
func assertSameMessage(t *testing.T, got, want string) {
	t.Helper()
	var gotFields, wantFields map[string]any
	if err := json.Unmarshal([]byte(got), &gotFields); err != nil {
		t.Fatalf("the child wrote %q, which is not a JSON object: %v", got, err)
	}
	if err := json.Unmarshal([]byte(want), &wantFields); err != nil {
		t.Fatalf("the expectation %q is not a JSON object: %v", want, err)
	}
	if !reflect.DeepEqual(gotFields, wantFields) {
		t.Fatalf("the child wrote %v, want %v", gotFields, wantFields)
	}
}

// waitForExit waits for the child to stop, or fails at the deadline.
func waitForExit(t *testing.T, cmd *exec.Cmd) {
	t.Helper()
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("the child exited badly: %v", err)
		}
	case <-time.After(childDeadline):
		cmd.Process.Kill()
		t.Fatalf("the child did not stop within %s of a shutdown", childDeadline)
	}
}

func TestARealChildAnswersOnTheChannel(t *testing.T) {
	child := startChild(t)

	assertSameMessage(t, readLineWithin(t, child.reader), `{"kind":"ready"}`)
	assertSameMessage(t, readLineWithin(t, child.reader), `{"kind":"metric","name":"rps","value":42.0}`)

	if _, err := child.writer.Write([]byte("{\"kind\":\"action\",\"name\":\"gc\",\"params\":\"now please\",\"id\":7}\n")); err != nil {
		t.Fatalf("send the action: %v", err)
	}
	assertSameMessage(t, readLineWithin(t, child.reader),
		`{"kind":"action-reply","action":"gc","body":"collected, fields=now,please","id":7}`)

	// The rule this module exists for, against a real process.
	if _, err := child.writer.Write([]byte("{\"kind\":\"action\",\"name\":\"typo\",\"id\":8}\n")); err != nil {
		t.Fatalf("send the unknown action: %v", err)
	}
	assertSameMessage(t, readLineWithin(t, child.reader),
		`{"kind":"action-reply","action":"typo","body":"unknown action: typo","id":8}`)

	if _, err := child.writer.Write([]byte("{\"kind\":\"shutdown\"}\n")); err != nil {
		t.Fatalf("send the shutdown: %v", err)
	}
	waitForExit(t, child.cmd)
}
```

- [ ] **Step 2: Write the unix harness**

`channel/harness_unix_test.go`:

```go
//go:build !windows

package channel

import (
	"bufio"
	"io"
	"net"
	"os"
	"os/exec"
	"syscall"
	"testing"
	"time"
)

// child is the shepherd's side of a supervised process.
type child struct {
	reader *bufio.Reader
	writer io.Writer
	cmd    *exec.Cmd
}

// startChild spawns the test binary as a supervised app on a real
// socketpair. It hands back the shepherd's end.
func startChild(t *testing.T) *child {
	t.Helper()
	pair, err := syscall.Socketpair(syscall.AF_UNIX, syscall.SOCK_STREAM, 0)
	if err != nil {
		t.Fatalf("socketpair: %v", err)
	}
	ourEnd := os.NewFile(uintptr(pair[0]), "shepherd-end")
	theirEnd := os.NewFile(uintptr(pair[1]), "app-end")

	cmd := exec.Command(os.Args[0])
	cmd.Env = append(os.Environ(),
		childGuard+"=1",
		FDVar+"=3",
		VersionVar+"="+Version,
		nameVar+"=answers",
	)
	// ExtraFiles[0] becomes the child's fd 3, which is where the
	// shepherd puts the channel.
	cmd.ExtraFiles = []*os.File{theirEnd}
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start the child: %v", err)
	}
	if err := theirEnd.Close(); err != nil {
		t.Fatalf("close the child's end in the parent: %v", err)
	}

	conn, err := net.FileConn(ourEnd)
	if err != nil {
		t.Fatalf("wrap the shepherd's end: %v", err)
	}
	if err := ourEnd.Close(); err != nil {
		t.Fatalf("close our duplicate: %v", err)
	}
	if err := conn.SetDeadline(time.Now().Add(childDeadline)); err != nil {
		t.Fatalf("set the deadline: %v", err)
	}
	t.Cleanup(func() {
		conn.Close()
		cmd.Process.Kill()
	})
	return &child{reader: bufio.NewReader(conn), writer: conn, cmd: cmd}
}
```

- [ ] **Step 3: Write the Windows harness**

The parent has to create the pipe server, which the standard library does not expose, so this reaches `CreateNamedPipeW` and `ConnectNamedPipe` through `syscall.NewLazyDLL` exactly as the production reader reaches `PeekNamedPipe`.

`channel/harness_windows_test.go`:

```go
//go:build windows

package channel

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"os/exec"
	"syscall"
	"testing"
	"time"
	"unsafe"
)

var (
	procCreateNamedPipeW = kernel32.NewProc("CreateNamedPipeW")
	procConnectNamedPipe = kernel32.NewProc("ConnectNamedPipe")
)

const (
	pipeAccessDuplex = 0x00000003
	pipeTypeByte     = 0x00000000
	pipeWait         = 0x00000000
	pipeBufferBytes  = 4096
	// errorPipeConnected means the client connected before
	// ConnectNamedPipe was called, which is a success.
	errorPipeConnected = syscall.Errno(535)
)

// child is the shepherd's side of a supervised process.
type child struct {
	reader *bufio.Reader
	writer io.Writer
	cmd    *exec.Cmd
}

// startChild creates one pipe instance and spawns the test binary
// against it. It hands back the shepherd's end.
//
// One instance, like the shepherd. The reader and the writer then share
// a handle, which is what the peek survives.
func startChild(t *testing.T) *child {
	t.Helper()
	name := fmt.Sprintf(`\\.\pipe\shep-go-channel-%d-%d`, os.Getpid(), time.Now().UnixNano())
	wide, err := syscall.UTF16PtrFromString(name)
	if err != nil {
		t.Fatalf("encode the pipe name: %v", err)
	}
	handle, _, callErr := procCreateNamedPipeW.Call(
		uintptr(unsafe.Pointer(wide)),
		pipeAccessDuplex,
		pipeTypeByte|pipeWait,
		1,
		pipeBufferBytes,
		pipeBufferBytes,
		0,
		0,
	)
	if handle == uintptr(syscall.InvalidHandle) {
		t.Fatalf("CreateNamedPipeW: %v", callErr)
	}
	server := os.NewFile(handle, name)
	t.Cleanup(func() { server.Close() })

	cmd := exec.Command(os.Args[0])
	cmd.Env = append(os.Environ(),
		childGuard+"=1",
		PipeVar+"="+name,
		VersionVar+"="+Version,
		nameVar+"=answers",
	)
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatalf("start the child: %v", err)
	}
	t.Cleanup(func() { cmd.Process.Kill() })

	connected := make(chan error, 1)
	go func() {
		reported, _, connectErr := procConnectNamedPipe.Call(handle, 0)
		if reported == 0 && connectErr != errorPipeConnected {
			connected <- connectErr
			return
		}
		connected <- nil
	}()
	select {
	case err := <-connected:
		if err != nil {
			t.Fatalf("ConnectNamedPipe: %v", err)
		}
	case <-time.After(childDeadline):
		t.Fatalf("the child never opened the pipe within %s", childDeadline)
	}

	return &child{reader: bufio.NewReader(server), writer: server, cmd: cmd}
}
```

A synchronous pipe handle has no deadline, so `SetReadDeadline` is not available here. The forcing mechanism is `readLineWithin`'s `select`, which is why it is shared rather than per platform.

- [ ] **Step 4: Run it**

Run: `go test -race -run TestARealChildAnswersOnTheChannel ./...`
Expected: PASS, on macOS and Linux directly, and on Windows once CI is up in Task 11 or on a real Windows host.

Then the whole suite, since `TestMain` now exists and every other test runs through it:

Run: `go test -race ./...`
Expected: PASS.

- [ ] **Step 5: Prove the harness is not vacuous**

In `discover`, return `Endpoint{Kind: EndpointAbsent}, nil` as the first statement. Confirm `TestARealChildAnswersOnTheChannel` fails on the first read at `childDeadline` rather than passing or hanging. Restore.

- [ ] **Step 6: Prove the Windows peek is not vacuous**

**On a real Windows machine, not in CI and not under emulation.** In `reader_windows.go`, replace `openPipe`'s reader with the file itself:

```go
	return &connection{reader: pipe, writer: pipe}, nil
```

Run `go test -race -run TestARealChildAnswersOnTheChannel ./...` and confirm it fails at `childDeadline` on the first read, because the child's reader goroutine parks inside `ReadFile` and holds the handle against its own `Ready` write. That is the deadlock the three probes measured, reproduced on demand. Restore the `&pipeReader{pipe: pipe}` wrapper and confirm the test passes again.

Record both results in the pull request body. A Windows arm that has only ever been compiled has been checked for spelling, not for behaviour, and shep already shipped one bug that proves the difference: shep-channel's named-pipe arm type-checked on every Windows CI run for as long as it existed and deadlocked the first time a process executed it.

- [ ] **Step 7: Commit**

```bash
git add channel/child_test.go channel/harness_unix_test.go channel/harness_windows_test.go
git commit -m "test(channel): drive a real child over a real socketpair and a real pipe"
```

---

## Task 11: CI, the example, and the release setup

**Repo: `shep-pm/shep-go`.**

**Files:**
- Create: `.github/workflows/test.yml`, `.github/workflows/commits.yml`, `.github/workflows/release-please.yml`
- Create: `release-please-config.json`, `.release-please-manifest.json`
- Create: `examples/answers/go.mod`, `examples/answers/main.go`

- [ ] **Step 1: Write the example app as its own module**

It is a module of its own so the published one stays standard-library-only and carries no `main` package a consumer never asked for. Task 12 builds this and runs it under a real shepherd.

`examples/answers/go.mod`:

```
module github.com/shep-pm/shep-go/examples/answers

go 1.24

require github.com/shep-pm/shep-go/channel v0.0.0

replace github.com/shep-pm/shep-go/channel => ../../channel
```

`examples/answers/main.go`:

```go
// Command answers is a supervised app that answers on the shepherd
// channel.
//
// Run it under shep with `channel = true` and `shep trigger answers gc`
// reaches the handler below.
package main

import (
	"fmt"
	"log"
	"os"
	"strings"
	"time"

	"github.com/shep-pm/shep-go/channel"
)

func main() {
	shepherd := channel.Serve()

	shepherd.OnAction("gc", func(a channel.Action) string {
		return "collected, fields=" + strings.Join(a.Fields(), ",")
	})
	shepherd.OnShutdown(func() {
		log.Print("the shepherd asked us to stop")
		os.Exit(0)
	})

	if err := shepherd.Ready(); err != nil {
		log.Fatalf("ready: %v", err)
	}
	log.Printf("channel active=%v stamp=%q", shepherd.Active(), shepherd.Version())

	for tick := 0; ; tick++ {
		shepherd.Metric("ticks", float64(tick))
		if dropped := shepherd.DroppedMetrics(); dropped > 0 {
			fmt.Println("dropped", dropped, "samples")
		}
		time.Sleep(time.Second)
	}
}
```

Build it once, from `examples/answers/`:

```bash
go build ./...
```
Expected: EXIT=0.

- [ ] **Step 2: Write the test workflow**

Pin each action to the commit SHA of the tag named in the comment beside it, the way shep pins its own. Look each one up rather than inventing it:

```bash
gh api repos/actions/checkout/git/refs/tags/v5 --jq .object.sha
```
```bash
gh api repos/actions/setup-go/git/refs/tags/v6 --jq .object.sha
```
```bash
gh api repos/crate-ci/typos/git/refs/tags/v1.50.1 --jq .object.sha
```

A tag ref that resolves to a tag object rather than a commit needs a second dereference; `--jq .object.type` says which, and `gh api repos/<owner>/<repo>/git/tags/<sha> --jq .object.sha` finishes the job.

`.github/workflows/test.yml`:

```yaml
name: test

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

jobs:
  channel:
    name: channel (${{ matrix.os }}, go ${{ matrix.go }})
    runs-on: ${{ matrix.os }}
    timeout-minutes: 15
    strategy:
      fail-fast: false
      matrix:
        # The floor and the current release. The floor is what go.mod
        # claims, so it is tested rather than assumed.
        os: [ubuntu-latest, macos-latest, windows-latest]
        go: ["1.24", "stable"]
    defaults:
      run:
        working-directory: channel
    steps:
      - uses: actions/checkout@<sha> # v5
      - uses: actions/setup-go@<sha> # v6
        with:
          go-version: ${{ matrix.go }}
      - name: gofmt
        # bash on every runner: the Windows default shell is pwsh, where
        # this is not a command.
        shell: bash
        run: test -z "$(gofmt -l .)"
      - name: vet
        run: go vet ./...
      # -race everywhere, deliberately. This module is two goroutines
      # around one shared handle. That is the shape the detector exists
      # for. It needs a C toolchain, which every runner image ships.
      - name: test
        run: go test -race ./...

  example:
    name: example
    runs-on: ubuntu-latest
    timeout-minutes: 10
    defaults:
      run:
        working-directory: examples/answers
    steps:
      - uses: actions/checkout@<sha> # v5
      - uses: actions/setup-go@<sha> # v6
        with:
          go-version: stable
      - name: build
        run: go build ./...

  typos:
    name: typos
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@<sha> # v5
      - uses: crate-ci/typos@<sha> # v1.50.1
```

If the `-race` step fails on the Windows leg for want of a C toolchain, do not drop the leg: add a step printing `go env CGO_ENABLED` and `gcc --version` and report what it says. A Windows leg without the race detector is the leg least able to afford losing it.

- [ ] **Step 3: Write the commits workflow**

Copy `.github/workflows/commits.yml` from shep and change three things: the tool named in the header comment is release-please rather than release-plz, the local hook sentence goes (this repository has no `.githooks`), and the accepted-type regular expression stays as it is. release-please and release-plz accept the same nine types, and a subject neither can parse contributes nothing either way.

- [ ] **Step 4: Write the release setup**

There is no registry to publish to. The Go module proxy fetches a tag, so a release here is a tag and a changelog and nothing else.

`release-please-config.json`:

```json
{
  "$schema": "https://raw.githubusercontent.com/googleapis/release-please/main/schemas/config.json",
  "separator": "/",
  "include-component-in-tag": true,
  "include-v-in-tag": true,
  "packages": {
    "channel": {
      "release-type": "go",
      "component": "channel",
      "changelog-path": "CHANGELOG.md"
    }
  }
}
```

`"separator": "/"` is the whole reason this file is not the default. A module living at `channel/` is fetched by the proxy under the tag `channel/v0.1.0`, and release-please's default separator would tag it `channel-v0.1.0`, which the proxy would never find.

`.release-please-manifest.json`:

```json
{
  "channel": "0.0.0"
}
```

Starting at `0.0.0` means the first `feat` produces `0.1.0`, which is the version this plan ships.

`.github/workflows/release-please.yml`:

```yaml
name: release-please

on:
  push:
    branches: [main]

permissions:
  contents: write
  pull-requests: write

jobs:
  release:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: googleapis/release-please-action@<sha> # v4
        with:
          config-file: release-please-config.json
          manifest-file: .release-please-manifest.json
```

- [ ] **Step 5: Push and read the result**

Push the branch and open the pull request. Then read what CI says rather than what this plan says: six matrix legs, the example, the typos job and the commit-subject job. The Windows legs are the only thing that has ever run `reader_windows.go`, and a local gate on any other platform gives no signal about it at all.

- [ ] **Step 6: Commit**

```bash
git add .github release-please-config.json .release-please-manifest.json examples
git commit -m "ci: test on three platforms and release by tag"
```

---

## Task 12: The release gate, under a real shepherd

**Repo: `shep-pm/shep-go`, driven from a real shep install.**

CI compiling the Windows path is not the same claim as the library working. This task is the one that says the module works, and it blocks the tag rather than following it.

The equivalent Rust verification drove an example app under a live daemon and exercised readiness, an action with params, an unknown action, a metric and a graceful stop. This does the same, twice: once on the development host, and once on real Windows.

- [ ] **Step 1: Write the Flockfile the gate uses**

Somewhere outside both repositories, in a directory short enough that `$SHEP_HOME`'s control socket path stays under the platform limit:

```toml
[[app]]
name = "answers"
script = "./answers"
channel = true
wait_ready = true
wait_ready_timeout = "10s"
shutdown_with_message = true
action_timeout = "5s"
```

`wait_ready = true` is what makes readiness a gate rather than a line in a log: if `Ready` never reaches the shepherd, `shep start` fails and says so.

- [ ] **Step 2: Run the gate on the development host**

Build the example for this host, from `examples/answers/`:

```bash
go build -o answers .
```

Then, one command at a time, from the directory holding the Flockfile:

```bash
shep start ./Flockfile
```
Expected: EXIT=0 and `answers` reaching `online`. A hang here is a readiness failure, not a slow start.

```bash
shep trigger answers gc "now please"
```
Expected: a reply body of `collected, fields=now,please`, well inside `action_timeout`.

```bash
shep trigger answers typo
```
Expected: `unknown action: typo`, promptly. This is the rule the module exists for, and the failure mode is a five second wait rather than an error.

```bash
shep bleats answers
```
Expected: the `channel active=true stamp="1"` line, and no `shep-channel:` warning.

```bash
shep stop answers
```
Expected: a graceful stop through the shutdown handler, with no `kill_timeout` escalation in the log.

Then check the metric arrived, by whatever the shepherd exposes for it on the version in use: `shep describe answers` and the daemon's debug log both name it.

- [ ] **Step 3: Run the same gate on real Windows**

Cross-build from the development host, or build on the Windows host itself:

```bash
GOOS=windows GOARCH=amd64 go build -o answers.exe .
```

Copy the binary to the Windows host, point a Flockfile at `answers.exe`, and run every command from Step 2 there. The Windows host is the one reachable over ssh; the shepherd on it needs to be a build carrying the named-pipe channel, so check `shep --version` against the shep checkout before drawing any conclusion from a failure.

Two things to watch that unix will not show:

The graceful stop is the whole point. Windows has no way to deliver anything SIGTERM-shaped, so `shutdown_with_message` is the only graceful stop an app can get there, and this step is the only place the module's part in it is exercised end to end.

A reply arriving late rather than not at all is the peek's failure mode, not its absence. Time the `shep trigger` and expect milliseconds; a reply that lands after several seconds means something is parking where it should be polling, even though the test passed.

- [ ] **Step 4: Record what ran**

Put the results in the pull request body: the commands, the platform each ran on, the shep version, and the Windows non-vacuity result from Task 10 Step 6. A green CI matrix and a green gate are two different claims and the body should make it obvious which one each line is.

- [ ] **Step 5: Commit anything the gate changed**

If the gate found nothing, there is nothing to commit and that is the expected outcome. If it found something, fix it with its own conventional commit and re-run the whole of Step 2 and Step 3 afterwards, not just the failing command.

---

## Before opening the pull request

- [ ] **In `shep-pm/shep`:** Task 1's commit rides on this branch with the plan file. It opens no pull request of its own; it goes in with whatever branch carries the wire-export work.

- [ ] **In `shep-pm/shep-go`:** one pull request from `feat/channel`, with a body naming what was verified and where. Its title is a conventional subject, because the repository's merge settings decide what release-please reads.

- [ ] Run the suite from a clean tree once before asking for review. A warm cache hides a stale artefact, and a from-scratch green is worth more than a warm one:

```bash
go clean -cache -testcache
```
```bash
go test -race ./...
```

- [ ] Read the CI result rather than the local gate. The local gate never compiles the other platform's arm and never runs it; six matrix legs do.

- [ ] Confirm `git diff` against `channel/wire.go` is empty for every commit after the one that created it. Plan 5's acceptance test is a zero diff against that file, and a well-meant reformat during a later task is the way that test starts failing for a reason nobody can find.

- [ ] Before merging the first release pull request release-please opens, read the version it proposes. It should be `0.1.0`, and the tag it will create should be `channel/v0.1.0`. A tag in any other shape is not reachable by the module proxy, and the wrong first tag on a public module is not something a second tag fixes.

- [ ] After the tag lands, prove the module is actually fetchable, from an empty directory outside both repositories:

```bash
GOFLAGS=-mod=mod go mod download github.com/shep-pm/shep-go/channel@v0.1.0
```
Expected: EXIT=0. A failure here means the tag shape is wrong, and the fix is another release rather than a retag.
