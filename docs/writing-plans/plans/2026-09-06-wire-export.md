# The shepherd-channel client libraries, plan 5 of 5, partial: the wire emitter

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development` to implement this task-by-task. Steps use `- [ ]` for tracking.

**Goal:** Make the Rust enums the source of the Go spelling of this wire, so that a change to `crates/shep-channel/src/wire.rs` either stops the workspace compiling or turns a committed file stale, and never reaches `shep-go` silently.

**Architecture:** No new crate, no new workspace member, no xtask. `crates/shep-channel/tests/wire_export.rs` is an integration test in exactly the shape `tests/fixtures.rs` already has: under `SHEP_CHANNEL_BLESS=1` it writes `crates/shep-channel/wire/channel.go`, and on every other run it compares the committed copy byte for byte and fails. One convention for both generated artifacts, and one gesture that regenerates both.

**Tech Stack:** Rust 1.88, edition 2024. `serde_json` is already an unconditional dev-dependency of the crate. Nothing is added to any manifest.

**Spec:** `docs/brainstorming/specs/2026-09-02-shep-client-libraries-design.md`, "The generator" section. The Go bytes this emitter must reproduce come from plan 2, `docs/writing-plans/plans/2026-09-06-shep-go-channel.md`, Task 2 Step 5.

**Base:** this worktree, branch `feat/shep-go-and-wire-export`, which already carries the two spec commits and is where plan 2's Task 1 also lands.

## Scope: the emitted file, and deliberately not the cross-repo half

The spec's generator section describes two halves. Only one of them is in scope here, and the other is deferred with a reason rather than forgotten.

| half | state |
|---|---|
| the fixture corpus, blessed from the real serde impls | **already shipped**, `crates/shep-channel/tests/fixtures.rs`. Out of scope, do not rewrite it. |
| the emitted Go wire file, and its staleness check | **this plan** |
| the workflow that opens pull requests in `shep-js`, `shep-py` and `shep-go` when the bytes change, and the credential it needs | **deferred until `shep-js` and `shep-py` exist** |

The deferral is the spec's own argument, not a shortcut. With one consumer, the zero-diff acceptance test covers one language, which is the weaker test the propagation machinery exists to avoid. A credential with contents and pull-requests write on three repositories is also a real piece of attack surface to add for a job that today has one target a human can watch. Until the other two libraries exist, a vendored copy going stale is caught by hand, and that is a real gap rather than a solved problem.

Nothing here makes the deferred half harder later. It is a workflow that runs the same bless command and pushes the result, and it wants the emitter to exist first.

## The two measured hazards

Both were measured on 2026-09-06 and both shape this plan.

**Hazard 1: Go and Rust disagree about how a float is spelled.** serde_json writes `{"kind":"metric","name":"rps","value":42.0}` and `encoding/json` writes `"value":42`. Both are valid, and serde_json reads `42` back as an `f64`. This plan does not solve that, because it is not this plan's problem: the emitter writes Go source, not JSON, and it never compares Go's output to anything. It is named here so that nobody reaches for a byte-for-byte encode comparison on the Go side when they read this file. Plan 2 owns the resolution: decode stays exact, encode is compared through `map[string]any`.

**Hazard 2: `omitempty` silently drops a zero metric.** With `Value float64` tagged `json:"value,omitempty"`, a metric of 0 marshals with no `value` key at all, and the shepherd reads that as a malformed frame and skips it. Two consequences reach this plan:

1. Every optional Go field in the emitted file is a **pointer**. `omitempty` on a pointer omits only `nil`. That is why `ChildMessage` is one string and five pointers, and it is a decision the emitter carries in a table rather than derives.
2. The corpus has no zero-valued metric, so all seven current fixtures pass while that bug ships. Task 1 adds one.

## Where this plan sits

Five plans. This is the last, and it is the one that stops the other four drifting.

| plan | repo | produces |
|---|---|---|
| 1, done | `shep-pm/shep` | `crates/shep-channel`, the fixture corpus, one definition of the wire |
| 2 | `shep-pm/shep-go` | the `channel` module, and the Go spelling of the wire this plan must reproduce |
| 3 | `shep-pm/shep-js` | `@shep-pm/channel` and `@shep-pm/cli` |
| 4 | `shep-pm/shep-py` | `shep-pm` and `shep-cli` |
| **5, this one** | `shep-pm/shep` | the wire emitter, its committed output, and the staleness check |

This plan runs after plan 2 on purpose. Its acceptance test is that the first bless produces a zero diff against the Go types plan 2 wrote by hand. If the two disagree, one of them is wrong, and finding that out before a tag exists is the whole point of the ordering.

Task 1 overlaps plan 2 by design. Plan 2's own Task 1 adds the same fixture, on this same branch, because the Go suite cannot be written without it. Whichever runs first does the work and the other verifies and moves on; Task 1 below says how to tell.

## Global constraints

- MSRV 1.88, edition 2024. `[lints] workspace = true` applies to test targets too, but every item this plan adds is private to the test binary, so `missing_docs` and `missing_debug_implementations` do not reach them. `tests/fixtures.rs` is the precedent.
- **No new dependency, no new manifest key.** `serde_json` is already an unconditional `[dev-dependencies]` entry of `shep-channel`, and the emitter needs the wire types and nothing else, so the test target needs no feature gate and compiles under `--no-default-features` as well.
- **One cargo shape: `-p shep-channel`.** Use `cargo test -p shep-channel --all-features` while iterating. Do not alternate with `--workspace` inside a task; the workspace shares one target-dir lock and switching shapes re-resolves features and rebuilds. `--workspace` belongs to the task gate at the end of each task, and the gate is where it stays.
- **`crates/shep-channel/wire/channel.go` is byte-frozen against plan 2's `channel/wire.go`.** 48 lines, 1642 bytes, tab indentation, one trailing newline. Do not reformat it, reorder its fields, rename its identifiers or edit its comments. It is written by the emitter and by nothing else.
- **The file lives inside the crate, so it ships in the published tarball.** That is deliberate: `env!("CARGO_MANIFEST_DIR")` is how `fixtures.rs` already resolves its corpus, and a path outside the crate would work locally and break in a packaged build. It costs under two kilobytes.
- **The two paths are spelled differently on purpose.** Here it is `crates/shep-channel/wire/channel.go`, because `wire/` is the directory of emitted files and a future `channel.ts` and `channel.py` sit beside it. In `shep-go` the same bytes are `channel/wire.go`, because there the directory is the Go package. Neither is a mistake to fix.
- **Comments follow IR-47:** a comment says only what the code cannot. No history, no rejected alternatives, no paraphrase of the next line. Line comments at four lines, doc comments at six body lines, sentences at sixteen words or fewer, no capitals for emphasis. The one exception is the `// Code generated ... DO NOT EDIT.` marker in the emitted Go, whose capitals are a token the Go toolchain matches on.
- **Conventional commit subjects on every commit**: `type(scope): summary`, with `!` on anything breaking. release-plz reads individual commits and drops what it cannot parse. Accepted types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `ci`, `chore`, `style`.
- **No em dashes or en dashes** in anything this plan produces, including commit messages.
- **Never write a home-directory path, a real name or a personal email** into a file, a commit message or a workflow. Repo-relative paths only.

## Plan snippets about existing code are approximations

Every code block below that shows code **being added** is a specification: write it as given unless it does not compile, and say so if it does not. The Rust in Task 2 was compiled, formatted, linted and run on 2026-09-06 against a copy of `crates/shep-channel/src/wire.rs`, and its output diffed against plan 2's frozen Go text with a zero diff, so it is known to work rather than believed to. Every block that describes code **already in the tree** was read the same day and may have moved. Grep for it rather than trusting a line number, and report the difference instead of quietly working around it.

## File structure

```
crates/shep-channel/
  fixtures/child-metric-zero.json   new, Task 1, or already there from plan 2's Task 1
  tests/fixtures.rs                 modify, Task 1
  tests/wire_export.rs              new, Task 2, the emitter and its two tests
  wire/channel.go                   new, Task 2, emitted and committed

.gitattributes                      modify, Task 2, force LF on the emitted file
docs/shepherd-channel.md            modify, Task 5
docs/decisions.md                   modify, Task 5
```

`wire/` is a new directory in the crate. Nothing compiles it and nothing imports it; it exists to be read by `shep-go` and by whoever vendors it next.

---

## Task 1: The zero-valued metric the corpus is missing

Hazard 2 in one fixture. Without it, every language's suite can pass while its encoder drops a metric of zero, because nothing in the corpus has a zero in it.

**This task is shared with plan 2, whose Task 1 is the same work on the same branch.** Run `ls crates/shep-channel/fixtures/child-metric-zero.json` first. If it exists, read it, confirm it holds exactly `{"kind":"metric","name":"idle","value":0.0}`, confirm `cargo test -p shep-channel --test fixtures` passes, and skip to Task 2 without a commit. If it does not, do the task.

**Files:**
- Modify: `crates/shep-channel/tests/fixtures.rs`
- Create: `crates/shep-channel/fixtures/child-metric-zero.json`

**Interfaces:**
- Produces: `crates/shep-channel/fixtures/child-metric-zero.json`, read verbatim by plans 2, 3 and 4.

- [ ] **Step 1: Add the case to the child table**

In `crates/shep-channel/tests/fixtures.rs`, inside `child_messages_match_their_fixtures`, add an entry to `cases` immediately after `child-metric`:

```rust
        (
            "child-metric-zero",
            ChildMessage::Metric {
                name: "idle".into(),
                value: 0.0,
            },
        ),
```

`idle` rather than a second `rps`, and it matters more than it looks. Plan 2 vendors this corpus and its Go table names the same value; two fixtures differing only in a number read as a copy-paste slip, and one named `idle` at `0.0` reads as the case it is.

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

`0.0` rather than `0` is serde's spelling of an `f64`, measured rather than assumed. That is hazard 1 sitting in the corpus, and it is why plan 2 compares this direction semantically.

- [ ] **Step 4: Run again without blessing**

Run: `cargo test -p shep-channel --test fixtures`
Expected: PASS, now comparing against the committed file.

- [ ] **Step 5: Prove the new fixture is load-bearing**

Edit `crates/shep-channel/fixtures/child-metric-zero.json`, changing `"value":0.0` to `"value":1.0`. Run the test and confirm `child_messages_match_their_fixtures` fails naming that path. Restore with `git checkout -- crates/shep-channel/fixtures/child-metric-zero.json`.

- [ ] **Step 6: Run the task gate**

Four commands, one at a time, each with `$?` read directly rather than through a pipe. In zsh a pipeline's `$?` is the last command's.

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
git add crates/shep-channel/fixtures/child-metric-zero.json crates/shep-channel/tests/fixtures.rs
git commit -m "test(channel): add a zero-valued metric to the wire corpus"
```

---

## Task 2: The emitter, and the Go file it writes

An integration test in the shape of `tests/fixtures.rs`: bless writes, every other run compares and fails.

**Files:**
- Modify: `.gitattributes`
- Create: `crates/shep-channel/tests/wire_export.rs`
- Create: `crates/shep-channel/wire/channel.go`

**Interfaces:**
- Consumes: `shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage}`.
- Produces: `crates/shep-channel/wire/channel.go`, byte-identical to `channel/wire.go` in `shep-pm/shep-go`.

- [ ] **Step 1: Force LF on the file before it exists**

This has to happen first, because git decides a file's line endings when it is added.

`.gitattributes` opens with `* text=auto`, so a `.go` file is a text file and a Windows client with `core.autocrlf=true`, which is what Git for Windows installs by default and what the `windows-latest` runners have, checks it out with CRLF. `fs::read_to_string` then reads CRLF, the emitter writes LF, and the byte comparison fails on every Windows leg with a diff on all 48 lines and nothing to do with the code under test.

That trap is already documented twice in `.gitattributes`, across five path entries, and its second paragraph says how it was found: once a Windows host actually ran the suite. The JSON corpus escapes it only because each fixture is one line with no trailing newline, so there is no LF to convert. This file has 48 of them.

Add at the end of `.gitattributes`:

```gitattributes
# Emitted by `crates/shep-channel/tests/wire_export.rs` with `\n`, then
# compared byte for byte against a runtime-built string. `text=auto` above
# checks this out CRLF on Windows: a diff on all 48 lines. `shep-go`
# vendors these bytes and gofmt wants LF.
crates/shep-channel/wire/channel.go text eol=lf
```

- [ ] **Step 2: Write the emitter and its two tests**

`crates/shep-channel/tests/wire_export.rs`. This exact source was compiled, `cargo fmt --check` clean, clippy clean under `-D warnings`, and run on 2026-09-06, and its output diffed against plan 2's frozen Go text with a zero diff:

```rust
//! The Go spelling of this crate's two wire enums.
//!
//! `github.com/shep-pm/shep-go/channel` vendors these bytes as
//! `channel/wire.go`. Neither match below has a wildcard arm. A new
//! variant stops this file compiling until Go's spelling is decided.
//!
//! Regenerate with
//! `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test wire_export`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use shep_channel::{CHANNEL_VERSION, ChildMessage, ShepherdMessage};

/// One Go constant for one `kind` string.
struct Kind {
    ident: &'static str,
    doc: &'static str,
    wire: &'static str,
}

/// One field of a Go struct, in declaration order.
struct Field {
    ident: &'static str,
    ty: &'static str,
    tag: &'static str,
}

/// The Go constant for a child message's kind.
///
/// No wildcard arm. A new variant fails to compile here first.
fn child_kind(message: &ChildMessage) -> Kind {
    match message {
        ChildMessage::Ready => Kind {
            ident: "KindReady",
            doc: "KindReady is the child's readiness signal.",
            wire: "ready",
        },
        ChildMessage::Metric { .. } => Kind {
            ident: "KindMetric",
            doc: "KindMetric is one metric sample from the child.",
            wire: "metric",
        },
        ChildMessage::ActionReply { .. } => Kind {
            ident: "KindActionReply",
            doc: "KindActionReply is the child's answer to one action.",
            wire: "action-reply",
        },
    }
}

/// The Go constant for a shepherd message's kind.
///
/// No wildcard arm, for the same reason as [`child_kind`].
fn shepherd_kind(message: &ShepherdMessage) -> Kind {
    match message {
        ShepherdMessage::Shutdown => Kind {
            ident: "KindShutdown",
            doc: "KindShutdown is the shepherd asking the app to stop.",
            wire: "shutdown",
        },
        ShepherdMessage::Action { .. } => Kind {
            ident: "KindAction",
            doc: "KindAction is the shepherd dispatching one custom action.",
            wire: "action",
        },
    }
}

/// One value per variant, carrying every optional field.
///
/// The guard test reads the keys these serialize to.
fn child_samples() -> Vec<ChildMessage> {
    vec![
        ChildMessage::Ready,
        ChildMessage::Metric {
            name: "rps".into(),
            value: 42.0,
        },
        ChildMessage::ActionReply {
            action: "gc".into(),
            body: "ok".into(),
            id: Some(7),
        },
    ]
}

/// One value per variant, carrying every optional field.
fn shepherd_samples() -> Vec<ShepherdMessage> {
    vec![
        ShepherdMessage::Shutdown,
        ShepherdMessage::Action {
            name: "gc".into(),
            params: Some("now".into()),
            id: 7,
        },
    ]
}

/// Every optional field is a pointer. `omitempty` on a value would drop a
/// metric of zero and an id of zero.
#[rustfmt::skip]
const CHILD_FIELDS: &[Field] = &[
    Field { ident: "Kind",   ty: "string",   tag: "kind" },
    Field { ident: "Name",   ty: "*string",  tag: "name,omitempty" },
    Field { ident: "Value",  ty: "*float64", tag: "value,omitempty" },
    Field { ident: "Action", ty: "*string",  tag: "action,omitempty" },
    Field { ident: "Body",   ty: "*string",  tag: "body,omitempty" },
    Field { ident: "ID",     ty: "*uint64",  tag: "id,omitempty" },
];

/// `Params` is a pointer because an absent one and an empty one are
/// different messages.
#[rustfmt::skip]
const SHEPHERD_FIELDS: &[Field] = &[
    Field { ident: "Kind",   ty: "string",  tag: "kind" },
    Field { ident: "Name",   ty: "*string", tag: "name,omitempty" },
    Field { ident: "Params", ty: "*string", tag: "params,omitempty" },
    Field { ident: "ID",     ty: "*uint64", tag: "id,omitempty" },
];

const CHILD_DOC: &str = "\
// ChildMessage is one line the app writes to the shepherd.
//
// Kind selects which other fields carry meaning. Each of those is a
// pointer. Dropping a zero would lose a metric of 0.
";

const SHEPHERD_DOC: &str = "\
// ShepherdMessage is one line the shepherd writes to the app.
//
// Kind selects which other fields carry meaning. An absent Params
// differs from an empty one, so it is a pointer too.
";

/// Pads both columns the way gofmt aligns a struct.
fn emit_struct(out: &mut String, doc: &str, name: &str, fields: &[Field]) {
    let ident_width = fields
        .iter()
        .map(|field| field.ident.len())
        .max()
        .unwrap_or(0);
    let type_width = fields.iter().map(|field| field.ty.len()).max().unwrap_or(0);
    out.push_str(doc);
    writeln!(out, "type {name} struct {{").expect("write to a String");
    for field in fields {
        writeln!(
            out,
            "\t{:ident_width$} {:type_width$} `json:{:?}`",
            field.ident, field.ty, field.tag
        )
        .expect("write to a String");
    }
    out.push_str("}\n");
}

/// The whole Go file, ending in one newline.
fn emit() -> String {
    let mut out = String::new();
    out.push_str("// Code generated by shep's wire exporter. DO NOT EDIT.\n");
    out.push_str("// Source: crates/shep-channel/src/wire.rs in github.com/shep-pm/shep.\n");
    out.push_str("\npackage channel\n\n");

    out.push_str("// Version is the value the shepherd exports as SHEP_CHANNEL_VERSION.\n");
    out.push_str("//\n");
    out.push_str("// A stamp, not a negotiation. An app can notice a wire it has never\n");
    out.push_str("// seen. It cannot ask for a different one.\n");
    writeln!(out, "const Version = {CHANNEL_VERSION:?}").expect("write to a String");
    out.push('\n');

    out.push_str("// The kinds carried in every message's \"kind\" field.\n");
    out.push_str("const (\n");
    let child = child_samples();
    let shepherd = shepherd_samples();
    // Two samples of one variant would declare the same constant twice,
    // and Go refuses a redeclaration.
    let mut declared: BTreeSet<&'static str> = BTreeSet::new();
    for kind in child
        .iter()
        .map(child_kind)
        .chain(shepherd.iter().map(shepherd_kind))
    {
        if !declared.insert(kind.ident) {
            continue;
        }
        writeln!(out, "\t// {}", kind.doc).expect("write to a String");
        writeln!(out, "\t{} = {:?}", kind.ident, kind.wire).expect("write to a String");
    }
    out.push_str(")\n\n");

    emit_struct(&mut out, CHILD_DOC, "ChildMessage", CHILD_FIELDS);
    out.push('\n');
    emit_struct(&mut out, SHEPHERD_DOC, "ShepherdMessage", SHEPHERD_FIELDS);
    out
}

fn wire_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("wire")
        .join("channel.go")
}

/// The wire key a Go field's `json` tag names.
fn json_name(field: &Field) -> &'static str {
    field.tag.split(',').next().expect("a tag names a key")
}

/// Checks one enum's encodings against the Go table it emits.
///
/// Every key reaches a field, every field is reached, and both agree on
/// order. Go emits struct fields in declaration order, and the corpus
/// compares key order.
fn check_keys<'a>(samples: impl Iterator<Item = (String, &'a str)>, fields: &[Field]) {
    let known: BTreeSet<String> = fields
        .iter()
        .map(|field| json_name(field).to_owned())
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (encoded, kind) in samples {
        assert!(
            encoded.starts_with(&format!(r#"{{"kind":"{kind}""#)),
            "the emitter and serde disagree about a kind: {encoded}"
        );
        let mut previous = 0;
        for field in fields {
            let key = json_name(field);
            let Some(at) = encoded.find(&format!(r#""{key}":"#)) else {
                continue;
            };
            assert!(at >= previous, "{key} is out of order in {encoded}");
            previous = at;
            seen.insert(key.to_owned());
        }
        let keys: BTreeSet<String> = serde_json::from_str::<serde_json::Value>(&encoded)
            .expect("decode")
            .as_object()
            .expect("a message encodes as an object")
            .keys()
            .cloned()
            .collect();
        assert!(
            keys.is_subset(&known),
            "a wire key has no Go field: {encoded}"
        );
    }
    assert_eq!(seen, known, "a Go field is not on the wire any more");
}

#[test]
fn the_committed_go_file_is_what_the_emitter_writes() {
    let emitted = emit();
    let path = wire_path();
    if std::env::var_os("SHEP_CHANNEL_BLESS").is_some() {
        fs::create_dir_all(path.parent().expect("wire/ has a parent")).expect("create wire dir");
        fs::write(&path, &emitted).expect("write the wire file");
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}. Run with SHEP_CHANNEL_BLESS=1 to create it.",
            path.display()
        )
    });
    assert_eq!(
        committed,
        emitted,
        "{} is stale. github.com/shep-pm/shep-go/channel vendors these bytes as channel/wire.go.",
        path.display()
    );
}

#[test]
fn every_wire_key_reaches_a_go_field_in_the_same_order() {
    check_keys(
        child_samples().iter().map(|sample| {
            (
                serde_json::to_string(sample).expect("encode"),
                child_kind(sample).wire,
            )
        }),
        CHILD_FIELDS,
    );
    check_keys(
        shepherd_samples().iter().map(|sample| {
            (
                serde_json::to_string(sample).expect("encode"),
                shepherd_kind(sample).wire,
            )
        }),
        SHEPHERD_FIELDS,
    );
}
```

Five things in there are decisions rather than style.

**`child_kind` and `shepherd_kind` have no wildcard arm.** That is the first tripwire and the reason `wire.rs` argues, in its own module doc, that both enums are deliberately exhaustive. A new variant fails to compile here before it can reach anything else. Task 4 proves it.

**The Go field tables are hand-written and the guard test enforces them.** A Go identifier and a Go type are human decisions that cannot be derived from a Rust field name, so `CHILD_FIELDS` and `SHEPHERD_FIELDS` are declarations. `every_wire_key_reaches_a_go_field_in_the_same_order` is what stops them drifting: every key the samples serialize must have a field, every field must be reached by some sample, and the two must agree on order. Go emits struct fields in declaration order and three fixtures compare key order, so order is a wire fact rather than a formatting one.

**The samples carry every optional field on purpose.** `ActionReply` has an id and `Action` has params, so the guard sees `id` and `params` at all. A sample that omitted them would let a stale field sit in the table forever.

**`#[rustfmt::skip]` on the two tables.** rustfmt explodes a struct literal to one field per line, which turns a six-row table into thirty-nine lines and hides the column order that the test exists to protect. The skip keeps one row per field.

**Where the first tripwire stops, stated rather than implied.** The compile error lands in `child_kind`, and the sample list is a separate function a few lines below it. An author who adds the match arm and the Go field but forgets the sample fails `every_wire_key_reaches_a_go_field_in_the_same_order` with `a Go field is not on the wire any more`, because nothing reaches the new field. The one case that slips through is a variant that adds no field at all and no sample: its `Kind` constant is simply missing from the Go file and nothing says so. That is narrow, and the compile error still puts the author in this file with the sample list on screen, which is what closes it in practice.

**`emit_struct` pads both columns to the widest entry.** That is what gofmt does to a struct, and it is why `Kind   string   ` has the spacing it has. Adding a longer field name or type re-pads the whole struct, which changes the committed bytes, which is correct.

- [ ] **Step 3: Run it to watch it fail**

Run: `cargo test -p shep-channel --test wire_export`
Expected: FAIL, one panic from `the_committed_go_file_is_what_the_emitter_writes` naming `wire/channel.go` as missing and telling you to bless. `every_wire_key_reaches_a_go_field_in_the_same_order` passes already, because it reads nothing from disk.

- [ ] **Step 4: Bless it**

Run: `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel --test wire_export`
Expected: PASS, and `crates/shep-channel/wire/channel.go` exists.

One variable blesses both artifacts, so `SHEP_CHANNEL_BLESS=1 cargo test -p shep-channel` regenerates the corpus and the wire file together. That is the reason for reusing the name rather than adding a second one.

- [ ] **Step 5: Check the bytes before trusting them**

```bash
wc -lc crates/shep-channel/wire/channel.go
```
Expected: `48` lines, `1642` bytes.

```bash
tail -c 1 crates/shep-channel/wire/channel.go | xxd
```
Expected: `0a`, exactly one trailing newline. A Go file without one is not gofmt output.

```bash
head -1 crates/shep-channel/wire/channel.go
```
Expected: `// Code generated by shep's wire exporter. DO NOT EDIT.` That is the form `gopls` and the Go toolchain match on, `^// Code generated .* DO NOT EDIT\.$`, and the blank line after the second comment line is what stops the marker becoming the package doc.

If `go` is installed, one more, and it is worth installing it for:

```bash
gofmt -l crates/shep-channel/wire/
```
Expected: no output. A filename here is a failure. Measured clean on 2026-09-06 with go1.26.5.

- [ ] **Step 6: Run again without blessing**

Run: `cargo test -p shep-channel --test wire_export`
Expected: PASS, now comparing against the committed file.

- [ ] **Step 7: Confirm the file ships**

Two commands, not one piped into the other: a pipe would hide cargo's own
exit status behind grep's, and in zsh a pipeline's `$?` is the last
command's. A cargo failure and a genuinely missing file must not read the
same.

```bash
cargo package -p shep-channel --list --allow-dirty > "${TMPDIR:-/tmp}/wire-package-list.txt"; echo "EXIT=$?"
```
Expected: `EXIT=0`. Anything else is cargo failing to list the package, and
the grep below means nothing until this is fixed.

```bash
grep -c '^wire/channel\.go$' "${TMPDIR:-/tmp}/wire-package-list.txt"
```
Expected: `1`. The crate manifest has no `include` key, so everything
tracked under the crate directory is packaged. `0` here, with the command
above already at `EXIT=0`, means the test resolves a path through
`CARGO_MANIFEST_DIR` that will not exist in a packaged build, and that is
a stop.

- [ ] **Step 8: Run the task gate**

The four commands from Task 1 Step 6, one at a time.

- [ ] **Step 9: Commit**

```bash
git add .gitattributes crates/shep-channel/tests/wire_export.rs crates/shep-channel/wire/channel.go
git commit -m "feat(channel): emit the Go spelling of the wire from the Rust enums"
```

The `.gitattributes` line rides in this commit rather than its own. It is meaningless without the file it protects, and separating them leaves one commit in history where a Windows checkout is already wrong.

---

## Task 3: The acceptance test, a zero diff against the hand-written Go

Task 2 blessed a file and then compared the file to what blessed it, which proves the mechanism and nothing about the bytes. This task is where the bytes are checked, against a source that did not come from the emitter.

**Files:** none. This task changes nothing and either passes or stops the plan.

- [ ] **Step 1: Get the hand-written Go**

If a `shep-pm/shep-go` checkout exists on this machine, use its `channel/wire.go` and say which commit you read. That is the authority.

If it does not, the same bytes are frozen in plan 2, in this repository. Extract the fenced block that starts with the generated marker:

````bash
awk '
  /^```go$/ { buf = ""; inblock = 1; next }
  inblock && /^```$/ { if (buf ~ /^\/\/ Code generated by shep/) { printf "%s", buf; exit } inblock = 0; next }
  inblock { buf = buf $0 "\n" }
' docs/writing-plans/plans/2026-09-06-shep-go-channel.md > "${TMPDIR:-/tmp}/wire-frozen.go"
````

```bash
wc -lc "${TMPDIR:-/tmp}/wire-frozen.go"
```
Expected: `48` lines, `1642` bytes. A different count means the extraction picked up the wrong block, and the diff below would be meaningless.

- [ ] **Step 2: Diff**

```bash
diff "${TMPDIR:-/tmp}/wire-frozen.go" crates/shep-channel/wire/channel.go && echo ZERO
```
Expected: `ZERO`, no output from `diff`. Measured on 2026-09-06 with this emitter against that block.

- [ ] **Step 3: If it is not zero, stop and decide which side is wrong**

Do not bless the emitter to match, and do not edit the Go by hand. Both moves make the diff go away and destroy the only signal this plan produces. Work out which side is wrong first:

- **A whitespace-only diff is the emitter's fault.** Column padding, a missing blank line, a trailing newline. `emit_struct` and `emit` are where it lives.
- **A field order, a pointer or a type diff is a design question, and plan 2 argued each one.** Every optional field is a pointer because of hazard 2. `ID` is `*uint64` for the same reason: an action id of zero under a plain `uint64` would be dropped from the reply and the shepherd would fall back to matching by name and order. `Name` and `Value` come before `Action` and `Body` so a metric encodes as `kind, name, value` and a reply as `kind, action, body, id`, which is what the corpus holds.
- **A comment or identifier diff is whichever side moved after the other was written.** Read plan 2's Task 2 Step 5, which states that every byte of that file is a decision, and its constraint that the file is byte-frozen from the moment it is created.

Fix the wrong side, re-bless if it was the emitter, and re-run this task. Record which side was wrong and why in the Task 5 decisions entry, because that is the finding this ordering exists to produce.

---

## Task 4: Both tripwires, proved by mutation

Two independent mechanisms catch two different mistakes, and neither is trusted until it has been watched going red. All four mutations below were run on 2026-09-06 and the expected output is what they actually printed.

**Files:** none permanently. Every step edits `crates/shep-channel/src/wire.rs` and restores it.

**Rule for the whole task:** after each restore, run `git diff --stat` and confirm it is empty before starting the next one. A half-restored mutation makes the next result a lie.

- [ ] **Step 1: Tripwire one, a new variant does not compile**

In `crates/shep-channel/src/wire.rs`, add a variant to `ChildMessage` above `ActionReply`:

```rust
    /// A new shape nobody has decided the Go spelling of
    Heartbeat,
```

Run: `cargo test -p shep-channel --test wire_export`
Expected: FAIL to compile, `error[E0004]: non-exhaustive patterns: &ChildMessage::Heartbeat not covered`, pointing at `child_kind`.

That is the property the two enums were left exhaustive for, spent rather than only described. The author of the new variant cannot get a green build without deciding what Go calls it, which is the review a change on this wire deserves.

Restore: `git checkout -- crates/shep-channel/src/wire.rs`

- [ ] **Step 2: Tripwire two, a renamed field is caught by the guard**

On `ChildMessage::ActionReply`'s `body` field, add:

```rust
        #[serde(rename = "text")]
```

Run: `cargo test -p shep-channel --test wire_export`
Expected: `the_committed_go_file_is_what_the_emitter_writes` passes and `every_wire_key_reaches_a_go_field_in_the_same_order` FAILS with:

```
a wire key has no Go field: {"kind":"action-reply","action":"gc","text":"ok","id":7}
```

Note which of the two failed, because it is the point of having both. The emitted bytes did not change, since the Go field table is a declaration rather than a derivation, so the staleness check alone would have passed this through. The guard is what catches a rename.

Restore: `git checkout -- crates/shep-channel/src/wire.rs`

- [ ] **Step 3: Tripwire two again, a reordered field is caught as well**

Swap the declaration order of `action` and `body` in `ChildMessage::ActionReply`, doc comments and all, so `body` comes first.

Run: `cargo test -p shep-channel --test wire_export`
Expected: `every_wire_key_reaches_a_go_field_in_the_same_order` FAILS with:

```
body is out of order in {"kind":"action-reply","body":"ok","action":"gc","id":7}
```

This one has no compiler behind it and no byte change either. Serde emits struct fields in declaration order, `encoding/json` does the same, and three fixtures compare key order, so a reorder in Rust silently makes the Go struct wrong. Nothing but this assertion sees it.

Restore: `git checkout -- crates/shep-channel/src/wire.rs`

- [ ] **Step 4: Tripwire one's other half, a value change makes the committed copy stale**

Change `CHANNEL_VERSION` from `"1"` to `"2"`.

Run: `cargo test -p shep-channel --test wire_export`
Expected: `every_wire_key_reaches_a_go_field_in_the_same_order` passes and `the_committed_go_file_is_what_the_emitter_writes` FAILS with `crates/shep-channel/wire/channel.go is stale. github.com/shep-pm/shep-go/channel vendors these bytes as channel/wire.go.`

Then run `cargo test -p shep-channel --test fixtures` with the same mutation still applied. Expected: PASS. No fixture carries the version stamp, so the corpus that has been guarding this wire since the crate landed is blind to a `CHANNEL_VERSION` change, and this emitter is the first thing in the workspace that is not. That is worth knowing before deciding this task was ceremony.

**Do not bless while this mutation is applied.** A bless here writes `const Version = "2"` into the committed file and the plan's acceptance test quietly stops meaning anything.

Restore: `git checkout -- crates/shep-channel/src/wire.rs`

- [ ] **Step 5: Confirm the tree is clean and green**

```bash
git status --porcelain
```
Expected: no output.

Run: `cargo test -p shep-channel --all-features`
Expected: PASS, both test targets.

---

## Task 5: Say it exists, and close the phase

**Files:**
- Modify: `docs/shepherd-channel.md`
- Modify: `docs/decisions.md`

- [ ] **Step 1: Confirm the CI claim rather than asserting it**

No new job is needed, and this was checked against `.github/workflows/test.yml` on 2026-09-06 rather than assumed. Read it again and confirm all four still hold:

1. A new file under `crates/shep-channel/tests/` is an integration test target cargo discovers on its own. Nothing lists test targets by name.
2. The `test` job runs `cargo nextest run --profile ci --workspace --locked --all-features` with a filter that excludes only `::slow::` and three named tests, none of them these. That is four operating systems by two toolchains. `features`, `musl`, `minimal-versions` and `coverage` each run the workspace tier as well, so this test runs in every one of them.
3. The `changes` job's `rust` filter includes `crates/**`, so an edit to `crates/shep-channel/wire/channel.go` on its own still expands the full Rust matrix. A generated file that could be edited without running the check that guards it would be worse than no check.
4. The `windows-latest` legs of `test` and `features` are the only place the `.gitattributes` entry from Task 2 is exercised. A local macOS run cannot fail that way, so a green local gate says nothing about it.

Write down what you confirmed. If any of the four has changed, that is a finding and the plan needs a wiring task rather than a sentence.

- [ ] **Step 2: Point the contract at the emitted file**

`docs/shepherd-channel.md` is the language-agnostic contract, and a Go author reading it should not have to retype these shapes. Under `## The wire format`, after the two subsections that list what you send and receive, add:

```markdown
The Go spelling of both shapes is generated from the Rust enums above and
committed at `crates/shep-channel/wire/channel.go`. It is the same file
`github.com/shep-pm/shep-go/channel` ships as `channel/wire.go`. Copy it
rather than retyping it: every optional field is a pointer, because Go's
`omitempty` on a plain value drops a metric of zero and an id of zero.
```

Do not restate the API and do not turn this document into Go documentation. Its whole value is being the contract every language reads.

- [ ] **Step 3: Record the two decisions that are not obvious from the diff**

In `docs/decisions.md`, a new section at the end, matching the file's existing shape including its `verified` footer:

```markdown
## The wire emitter

### The Go types are emitted by a test, and the guard against drift is two mechanisms rather than one

An exhaustive match catches a new variant and nothing else. It is a compile
error in `child_kind` or `shepherd_kind` the moment a variant lands, which is
the property `wire.rs` argues for in its own module doc and this is what
spends it. What it cannot see is a rename, a reorder or a serde attribute,
because all three still compile.

So the emitter carries a hand-written Go field table and a test that holds it
against serde. A Go identifier and a Go type are human decisions and cannot be
derived from a Rust field name; what can be derived is the set of keys the
types actually serialize and the order they come out in, and that is what the
table is checked against. Measured on 2026-09-06: renaming `body` to `text`
leaves the emitted bytes identical and fails only the guard, and swapping
`action` and `body` in the declaration does the same. Neither is visible to a
byte comparison, and a reorder is not visible to the compiler either.

The staleness check is the third leg and it covers what the emitter
interpolates rather than declares. `CHANNEL_VERSION` is the live example:
changing it to `"2"` leaves every fixture passing, because no fixture carries
the stamp, and turns the committed Go file stale immediately.

The emitted file lives inside the crate so `CARGO_MANIFEST_DIR` resolves it in
a packaged build the way `fixtures.rs` already does, which means it ships in
the tarball. Under two kilobytes, accepted.

`verified crates/shep-channel/tests/wire_export.rs (child_kind, CHILD_FIELDS, every_wire_key_reaches_a_go_field_in_the_same_order, the_committed_go_file_is_what_the_emitter_writes), crates/shep-channel/wire/channel.go, .gitattributes (the eol=lf entry)`

### The cross-repo propagation half is deferred until three libraries exist

The spec's generator section describes a workflow that opens a pull request in
`shep-js`, `shep-py` and `shep-go` when the wire bytes change, and a
credential with contents and pull-requests write on all three. Neither ships
here. With one consumer the zero-diff acceptance test covers one language,
which is the weaker test that machinery exists to avoid, and a token that can
write to three repositories is real attack surface to add for a job with one
target a person can watch.

Until then a vendored copy going stale is caught by hand. That is a gap rather
than a solved problem, and it is written down here so nobody later reads the
emitter as the whole of what was designed.

`verified docs/brainstorming/specs/2026-09-02-shep-client-libraries-design.md (The generator)`
```

- [ ] **Step 4: Point the public docs page at the emitted file too**

This plan adds no verb, no flag, no alias, no `shep.toml` key, no Flockfile field, no exit code and no JSON payload shape, so the generated CLI reference is untouched. One prose page still needs the pointer Step 2 adds to the markdown contract:

```bash
grep -rln "shepherd channel\|SHEP_CHANNEL" web/src/pages/
```

`web/src/pages/docs/shepherd-channel.astro` is on that list. Its own header comment says it is ported from `docs/shepherd-channel.md`, so the pointer Step 2 puts in the markdown belongs here too. Read it and confirm the paragraph below still ends where this shows, then add the second paragraph after it:

```astro
    <p>
      <strong>A Rust app does not have to speak this by hand.</strong> The{" "}
      <code>shep-channel</code> crate implements everything on this page:
      discovering the descriptor, framing the JSON, and answering messages
      the app does not handle itself. Go, JavaScript and Python libraries
      over the same contract are planned. Whatever language an app is
      written in, this page is still the contract it has to hold to.
    </p>
    <p>
      <strong>Go types are generated from the Rust enums, not typed by
      hand.</strong> The committed file is{" "}
      <code>crates/shep-channel/wire/channel.go</code> in this repository,
      and <code>github.com/shep-pm/shep-go/channel</code> vendors the same
      bytes as <code>channel/wire.go</code>. Copy it rather than retyping
      it: every optional field is a pointer, because Go's{" "}
      <code>omitempty</code> on a plain value drops a metric of zero and
      an id of zero.
    </p>

    <h2>The wire format</h2>
```

If the surrounding text has moved, add the same paragraph directly after whichever one introduces the language libraries, and report the difference rather than guessing a new spot.

Then run both, because `astro build` does not typecheck and a wrong prop renders wrong while building clean:

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

- [ ] **Step 5: Run the phase gate**

The four task-gate commands, then the serial run. It is not ceremony: it was red on `main` before Phase 5 and it caught a real regression in Phase 6.

```bash
cargo test --workspace --all-features -- --test-threads=1
```

- [ ] **Step 6: Commit**

```bash
git add docs/shepherd-channel.md docs/decisions.md
git commit -m "docs(channel): point the contract at the emitted Go types"
```

---

## Before opening the pull request

- [ ] The two cross-checks from `CLAUDE.md`, once for this branch rather than once per task, each with its own `CARGO_TARGET_DIR` if you want the host cache left alone:

```bash
cargo check -p shep-daemon --all-targets --all-features --target x86_64-unknown-linux-gnu
```
```bash
cargo check --workspace --all-targets --all-features --target x86_64-pc-windows-gnu
```

The Windows one needs `brew install mingw-w64`, because `ring`'s build script runs `cc` for the target.

- [ ] **Read the CI result, and read the Windows legs specifically.** The local gate cannot fail the way a CRLF checkout fails, so `.gitattributes` is only ever proved on `windows-latest`. A green macOS run says nothing about it.

- [ ] Confirm `git diff` against `crates/shep-channel/wire/channel.go` is empty for every commit after the one that created it. Plan 2 froze those bytes and a well-meant reformat here is how that test starts failing for a reason nobody can find.

- [ ] Confirm `crates/shep-channel/fixtures/` holds eight files and that plan 2's Task 1 and this plan's Task 1 did not each add one. Two zero-metric fixtures under different names would be vendored into three repositories.

- [ ] Say in the pull request body that the cross-repo propagation workflow and its credential are deliberately not here, and why. A reviewer holding the spec will otherwise read this branch as the generator arriving half-built, which is the opposite of what the deferral decided.
