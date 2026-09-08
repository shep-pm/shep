# .env import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `shep import env <FILE> --app <NAME>` reads a `.env`, puts the keys the operator names into the secret store, puts the rest into that sheep's own env overrides, and refuses whole rather than half finishing, up to the point where it starts writing. The one window where that stops being true is between the secret write and the env batch: the design doc's decision 13 says what reaches it and what it leaves behind.

**Architecture:** `shep import` splits into `pm2` and `env` subcommands, which forces the existing `import/` modules under an `import/pm2/` directory and gives the new half `import/dotenv/`. The parser is hand written and strict. Classification is a glob match against the parsed keys. The secret half is written by the CLI directly, as `shep secret set` already is; the env half goes through one new additive request, `Request::SetSheepEnvBatch`, which the daemon applies as a single read-modify-write of `overrides.json` so every key lands or none does.

**Tech Stack:** Rust 1.88, edition 2024, clap, globset (already a workspace dependency), serde_json.

**Spec:** [docs/brainstorming/specs/2026-09-07-dotenv-import-design.md](../../brainstorming/specs/2026-09-07-dotenv-import-design.md)

## Global Constraints

- `PROTOCOL_VERSION` stays at **8** and `MIN_SUPPORTED` stays at **8**. `Request::SetSheepEnvBatch` and `Response::SheepEnvBatch` are additive, and `Request::Unrecognized` already makes an older daemon refuse an unknown variant by name. If a task appears to require a bump, stop and report rather than bumping.
- `SCHEMA_VERSION` is untouched.
- Conventional commit subjects, `type(scope): summary`. Exactly one commit in this plan takes a `!`: Task 1, which removes the bare `shep import` form. It lands in `shep`, the crate that breaks.
- Invoke the `shep-idiomatic-rust` skill before writing Rust. Cite `IR-<n>` where a rule applies.
- Every new public item needs docs and a deliberate `Debug` decision. Anything that can hold a `.env` value gets a redacted `Debug` with an exact-string test (IR-41).
- **No value from a `.env` may reach stdout, stderr, a log, an error message, or a process argument.** Errors name the key, the line and the file. `--dry-run` prints a byte length. `shep secret get` stays the only path by which a stored value reaches stdout.
- One cargo shape for the whole plan: `--workspace`. Do not alternate with `-p <crate>`; it churns this repo's build cache badly.
- Iterate with `cargo test --workspace --all-features --lib --bins`. Run the full `cargo test --workspace --all-features` once per task, before committing.
- **Never touch `~/.shep`.** Any run against a live daemon points `SHEP_HOME` at a fresh `mktemp -d`, which must be short: a long path exceeds `SUN_LEN` for the control socket.
- The repo runs a spelling check on commits. Do not put a deliberately misspelled example in a doc comment or a commit message.

---

## File Structure

| File | Responsibility in this plan |
|---|---|
| `crates/shep-cli/src/cli.rs` | `ImportArgs` becomes a subcommand host; `ImportPm2Args` and `ImportEnvArgs` |
| `crates/shep-cli/src/commands/import/mod.rs` | dispatch between the two subcommands |
| `crates/shep-cli/src/commands/import/pm2/` | the existing `dump`, `convert`, `render`, `env` modules and today's `import` function, moved unchanged |
| `crates/shep-cli/src/commands/import/dotenv/parse.rs` | the `.env` grammar. Pure, holds no shep concepts |
| `crates/shep-cli/src/commands/import/dotenv/plan.rs` | `--only` and `--secret` classification, shep's key and value limits, the warning list |
| `crates/shep-cli/src/commands/import/dotenv/mod.rs` | the verb: read, parse, plan, connect, write |
| `crates/shep-cli/src/output/rows.rs` | `ImportEnvRow` and `ImportEnvRows` |
| `crates/shep-cli/src/lib.rs` | dispatch, moved into the connecting group for `env` |
| `crates/shep-cli/Cargo.toml` | `globset` |
| `crates/shep-core/src/protocol/request.rs` | `Request::SetSheepEnvBatch`, `Response::SheepEnvBatch` |
| `crates/shep-daemon/src/supervisor.rs` | `Command::SetSheepEnvBatch`, `set_sheep_env_batch`, `handle_set_sheep_env_batch`, `EnvBatch` |
| `crates/shep-daemon/src/rpc.rs` | the request arm |
| `crates/shep-cli/tests/cli_e2e.rs` | the pm2 rename, and the new verb against a live daemon |
| `web/src/data/cli-reference.generated.txt` | regenerated |
| `web/src/pages/docs/{from-pm2,first-flockfile,startup,secrets,overrides}.astro`, `web/src/components/landing/Features.astro`, `crates/shep-cli/README.md` | the published contract |

**Dependency order.** Task 1 first, alone. Tasks 2 and 3 (the parser, then the planner) are independent of Tasks 4 and 5 (the wire, then the daemon), and the two legs can run in parallel. Task 6 needs all of 2 through 5. Task 7 needs 1 and 6.

---

## Task 1: Split `shep import` into `pm2` and `env`

Today's verb becomes `shep import pm2` with its flags unchanged. Bare `shep import` stops working. `env` is not added here: this task is the move, so a reviewer can see that nothing about the pm2 path changed.

**Files:**
- Modify: `crates/shep-cli/src/cli.rs` (`ImportArgs`, around line 1313; the `Import` doc comment, around line 605)
- Move: `crates/shep-cli/src/commands/import/{dump,convert,render,env}.rs` and `testdata/` into `crates/shep-cli/src/commands/import/pm2/`
- Create: `crates/shep-cli/src/commands/import/pm2/mod.rs` (today's `import/mod.rs` body)
- Modify: `crates/shep-cli/src/commands/import/mod.rs` (becomes dispatch only)
- Modify: `crates/shep-cli/src/lib.rs` (dispatch arm, around line 1031; `import_parses_to_its_own_command`, around line 1943)
- Test: `crates/shep-cli/tests/cli_e2e.rs` (existing `shep import` cases)

**Interfaces:**
- Consumes: nothing.
- Produces: `cli::ImportArgs { command: ImportCommand }`, `cli::ImportCommand::Pm2(ImportPm2Args)`, `cli::ImportPm2Args` with the four fields `from: Option<PathBuf>`, `out: Option<PathBuf>`, `dry_run: bool`, `force: bool`. `commands::import::import(streams, args) -> ExitCode` keeps its name and its signature and dispatches. `commands::import::pm2::import(streams, args: &ImportPm2Args) -> ExitCode` is today's function.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/src/lib.rs`, replace `import_parses_to_its_own_command`:

```rust
/// Pins clap's parse only. An arm that parses correctly and calls the
/// wrong function needs a real invocation, which `cli_e2e.rs` covers.
#[test]
fn import_pm2_parses_to_its_own_subcommand() {
    use clap::Parser;
    use cli::{Commands, ImportCommand};
    let cli = Cli::try_parse_from(["shep", "import", "pm2"]).unwrap();
    let Commands::Import(args) = cli.command else {
        panic!("`shep import pm2` did not reach the import verb");
    };
    assert!(matches!(args.command, ImportCommand::Pm2(_)));
}

/// The bare form was `shep import` for the whole of 0.1 through 0.6 and
/// now names a subcommand. A refusal is the whole point of the split, so
/// it is pinned rather than left to clap.
#[test]
fn bare_import_no_longer_parses() {
    use clap::Parser;
    assert!(
        Cli::try_parse_from(["shep", "import"]).is_err(),
        "bare `shep import` must name a subcommand"
    );
}
```

In `crates/shep-cli/tests/cli_e2e.rs`, find every case invoking `import` (search for `"import"`) and put `"pm2"` after it. The two that matter are the dry-run case around line 3627 and the round-trip case around line 3645.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins import
```

Expected: FAIL, `ImportCommand` not found.

- [ ] **Step 3: Move the modules**

```bash
cd crates/shep-cli/src/commands/import
mkdir pm2
git mv dump.rs convert.rs render.rs env.rs testdata pm2/
git mv mod.rs pm2/mod.rs
```

Then create `crates/shep-cli/src/commands/import/mod.rs`:

```rust
//! `shep import`: reading somebody else's file into shep's own state.
//!
//! Two subcommands with nothing in common but a noun. [`mod@pm2`] reads a
//! `dump.pm2` and writes a Flockfile, connecting to nothing. [`mod@dotenv`]
//! reads a `.env` into the secret store and one sheep's env, which needs a
//! running shepherd. They are separate verbs because their inputs, their
//! outputs and their flags are all separate; see the design doc.

pub(crate) mod pm2;

use crate::cli::{ImportArgs, ImportCommand};
use crate::exit::ExitCode;
use crate::output::Streams;

/// `shep import <subcommand>`.
pub fn import(streams: &mut Streams<'_>, args: &ImportArgs) -> ExitCode {
    match &args.command {
        ImportCommand::Pm2(args) => pm2::import(streams, args),
    }
}
```

In `pm2/mod.rs`, change the module header's first line to `//! `shep import pm2`: reading a pm2 dump into a Flockfile.`, change `use crate::cli::ImportArgs;` to `use crate::cli::ImportPm2Args;`, and change `pub fn import(streams: &mut Streams<'_>, args: &ImportArgs)` to take `&ImportPm2Args`. The three `use crate::commands::import::dump;` lines inside the moved test modules become `use crate::commands::import::pm2::dump;`.

In `crates/shep-cli/src/cli.rs`, rename the existing struct and add the host:

```rust
/// Arguments to `shep import`.
///
/// A subcommand host rather than a flag set, for [`SecretArgs`]' reason: the
/// two inputs share a noun and nothing else. `Debug` is derived; neither
/// subcommand carries a value.
#[derive(Debug, clap::Args)]
pub struct ImportArgs {
    /// Which kind of file to read.
    #[command(subcommand)]
    pub command: ImportCommand,
}

/// `shep import`'s subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum ImportCommand {
    /// Write a Flockfile from a pm2 dump. Starts nothing.
    Pm2(ImportPm2Args),
}

/// Arguments to `shep import pm2`.
#[derive(Debug, clap::Args)]
pub struct ImportPm2Args {
    /// Read this pm2 dump instead of `~/.pm2/dump.pm2`
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// Write the Flockfile here instead of `./Flockfile.toml`
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Print the Flockfile that would be written, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite an existing Flockfile
    #[arg(long)]
    pub force: bool,
}
```

Move the long `Import(ImportArgs)` doc comment in `Commands` (around line 605) down onto `ImportCommand::Pm2`, since every sentence in it is about the pm2 dump. Leave `Commands::Import` with a one-line doc: `/// Read somebody else's config into shep: a pm2 dump, or a `.env`.`

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --lib --bins import
```

Expected: PASS.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```

- [ ] **Step 6: Commit**

```bash
git add -A crates/shep-cli
git commit
```

Subject: `feat(cli)!: split shep import into pm2 and env subcommands`. The body says the bare form is gone, names the four flags that moved unchanged, and says `env` arrives in a later commit.

---

## Task 2: The `.env` parser

Pure. Takes a string, returns entries or one error naming a line. Knows nothing about secrets, sheep or stores.

**Files:**
- Create: `crates/shep-cli/src/commands/import/dotenv/parse.rs`
- Create: `crates/shep-cli/src/commands/import/dotenv/mod.rs` (module declaration only for now)
- Modify: `crates/shep-cli/src/commands/import/mod.rs` (add `pub(crate) mod dotenv;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub(crate) struct Entry { pub key: String, pub value: String, pub line: usize }`, redacted `Debug`.
  - `pub(crate) enum ParseReason { NoEquals, EmptyKey, UnterminatedQuote, BadEscape, AmbiguousComment, TrailingText, Duplicate { first: usize } }`
  - `pub(crate) struct ParseError { pub line: usize, pub reason: ParseReason }`, `Display`, `core::error::Error`.
  - `pub(crate) fn parse(text: &str) -> Result<Vec<Entry>, ParseError>`

- [ ] **Step 1: Write the failing tests**

Create `crates/shep-cli/src/commands/import/dotenv/parse.rs` with only its test module to begin with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn keys_and_values(text: &str) -> Vec<(String, String)> {
        parse(text)
            .expect("this fixture parses")
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect()
    }

    #[test]
    fn plain_pairs_comments_and_blanks() {
        let parsed = keys_and_values("# a note\n\nNODE_ENV=production\nPORT=8080\n");
        assert_eq!(
            parsed,
            [
                ("NODE_ENV".to_string(), "production".to_string()),
                ("PORT".to_string(), "8080".to_string()),
            ]
        );
    }

    #[test]
    fn export_prefix_and_spaces_around_the_equals() {
        let parsed = keys_and_values("export NODE_ENV=production\nPORT = 8080\n");
        assert_eq!(parsed[0].0, "NODE_ENV");
        assert_eq!(parsed[1], ("PORT".to_string(), "8080".to_string()));
    }

    #[test]
    fn a_trailing_comment_is_taken_only_after_a_value_with_no_spaces() {
        assert_eq!(keys_and_values("PORT=8080 # dev\n")[0].1, "8080");
        assert_eq!(keys_and_values("KEY=v#3\n")[0].1, "v#3");
        assert_eq!(keys_and_values("NODE_OPTIONS=--a --b\n")[0].1, "--a --b");
        assert_eq!(keys_and_values("KEY= # note\n")[0].1, "");
    }

    #[test]
    fn an_ambiguous_trailing_comment_refuses() {
        let err = parse("PASSPHRASE=correct horse battery #4\n").unwrap_err();
        assert_eq!(err.line, 1);
        assert!(matches!(err.reason, ParseReason::AmbiguousComment));
        assert!(
            !err.to_string().contains("correct horse"),
            "the message must not carry the value: {err}"
        );
    }

    #[test]
    fn single_quotes_are_literal_and_must_close_on_the_line() {
        assert_eq!(keys_and_values("KEY='a \\n b # c'\n")[0].1, "a \\n b # c");
        let err = parse("KEY='unclosed\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::UnterminatedQuote));
    }

    #[test]
    fn double_quotes_take_escapes_and_may_span_lines() {
        assert_eq!(keys_and_values(r#"KEY="a\nb\"c\\d""#)[0].1, "a\nb\"c\\d");
        assert_eq!(
            keys_and_values("PEM=\"-----BEGIN-----\nline two\n-----END-----\"\n")[0].1,
            "-----BEGIN-----\nline two\n-----END-----"
        );
        let err = parse("KEY=\"a\\qb\"\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::BadEscape));
    }

    #[test]
    fn a_dollar_is_literal() {
        assert_eq!(keys_and_values("TOKEN=${PART}-live\n")[0].1, "${PART}-live");
        assert_eq!(keys_and_values("TOKEN=\"${PART}-live\"\n")[0].1, "${PART}-live");
    }

    #[test]
    fn a_duplicate_key_refuses_and_names_both_lines() {
        let err = parse("A=1\nB=2\nA=3\n").unwrap_err();
        assert_eq!(err.line, 3);
        assert!(matches!(err.reason, ParseReason::Duplicate { first: 1 }));
    }

    #[test]
    fn a_bom_and_crlf_are_stripped() {
        let parsed = keys_and_values("\u{feff}A=1\r\nB=2\r\n");
        assert_eq!(
            parsed,
            [
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn a_line_with_no_equals_refuses() {
        let err = parse("NODE_ENV production\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::NoEquals));
    }

    #[test]
    fn text_after_a_closing_quote_refuses() {
        let err = parse("KEY=\"a\" b\n").unwrap_err();
        assert!(matches!(err.reason, ParseReason::TrailingText));
    }

    /// IR-41. A derived `Debug` would print the value in a panic message,
    /// a test failure or a `dbg!`.
    #[test]
    fn entry_debug_does_not_leak() {
        let entry = Entry {
            key: "DB_PASSWORD".to_string(),
            value: "hunter2".to_string(),
            line: 4,
        };
        assert_eq!(
            format!("{entry:?}"),
            "Entry { key: \"DB_PASSWORD\", value: <7 bytes>, line: 4 }"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins dotenv::parse
```

Expected: FAIL, `parse` not found.

- [ ] **Step 3: Write the parser**

Above the test module in the same file:

```rust
//! The `.env` grammar, and nothing else.
//!
//! Strict on purpose. Every shape this file does not recognise refuses with
//! a line number rather than being guessed at, because the values are
//! credentials and a wrong guess is silent. No variable interpolation: `$`
//! is a literal character everywhere, which is the one place this parser
//! deliberately differs from most `.env` readers. dotenvy resolves `${NAME}`
//! against the reading process's own environment and turns an undefined name
//! into an empty string, which on this path would store a truncated secret.
//!
//! The trailing-comment rule is the subtle one and it is in
//! [`unquoted_value`].

use core::fmt;
use std::collections::BTreeMap;

/// One `KEY=value` pair.
///
/// `Debug` prints the value's length and never the value (IR-41).
/// Exact-string-tested below (`entry_debug_does_not_leak`).
pub(crate) struct Entry {
    /// The key, exactly as written.
    pub key: String,
    /// The value, unquoted and unescaped.
    pub value: String,
    /// The 1-based line the key was on.
    pub line: usize,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .field("line", &self.line)
            .finish()
    }
}

/// Why one line refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ParseReason {
    /// The line has no `=`.
    NoEquals,
    /// The name before the `=` is empty.
    EmptyKey,
    /// A quote opened and the file ended, or a single quote opened and its
    /// line ended.
    UnterminatedQuote,
    /// A backslash inside double quotes followed by something other than
    /// `n`, `r`, `t`, `"` or `\`.
    BadEscape,
    /// A ` #` in an unquoted value whose value already contains whitespace,
    /// so the line reads two ways.
    AmbiguousComment,
    /// Something other than whitespace or a comment after a closing quote.
    TrailingText,
    /// A key that an earlier line already set.
    Duplicate {
        /// The line that set it first.
        first: usize,
    },
}

/// One line refused, and why.
///
/// `Debug` is derived: no variant carries a value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParseError {
    /// The 1-based line.
    pub line: usize,
    /// What was wrong with it.
    pub reason: ParseReason,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.reason {
            ParseReason::NoEquals => f.write_str("no `=`; every line is `KEY=value`"),
            ParseReason::EmptyKey => f.write_str("the name before the `=` is empty"),
            ParseReason::UnterminatedQuote => f.write_str("a quote opened and never closed"),
            ParseReason::BadEscape => f.write_str(
                "a backslash inside double quotes must be followed by n, r, t, \\\" or \\\\",
            ),
            ParseReason::AmbiguousComment => f.write_str(
                "a ` #` after a value that already has spaces in it reads two ways; \
                 quote the value if the `#` is part of it",
            ),
            ParseReason::TrailingText => f.write_str("unexpected text after the closing quote"),
            ParseReason::Duplicate { first } => {
                write!(f, "this key was already set on line {first}")
            }
        }
    }
}

impl core::error::Error for ParseError {}

/// Reads a `.env` into its pairs.
///
/// # Errors
/// [`ParseError`] for the first line this grammar does not accept. No error
/// carries a value.
pub(crate) fn parse(text: &str) -> Result<Vec<Entry>, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();

    let mut entries = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line_number = index + 1;
        let line = lines[index].trim_start();
        if line.is_empty() || line.starts_with('#') {
            index += 1;
            continue;
        }
        let body = line
            .strip_prefix("export ")
            .map_or(line, str::trim_start);
        let Some((raw_key, rest)) = body.split_once('=') else {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::NoEquals,
            });
        };
        let key = raw_key.trim_end().to_string();
        if key.is_empty() {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::EmptyKey,
            });
        }
        if let Some(first) = seen.get(&key) {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::Duplicate { first: *first },
            });
        }
        let (value, extra_lines) = read_value(rest, &lines[index + 1..], line_number)?;
        seen.insert(key.clone(), line_number);
        entries.push(Entry {
            key,
            value,
            line: line_number,
        });
        index += 1 + extra_lines;
    }
    Ok(entries)
}

/// Reads one value, returning it and how many further lines it consumed.
///
/// `rest` is everything after the `=`, untrimmed, so that `KEY= # note`
/// still sees the space that makes the `#` a comment.
fn read_value(
    rest: &str,
    following: &[&str],
    line_number: usize,
) -> Result<(String, usize), ParseError> {
    let trimmed = rest.trim_start();
    match trimmed.as_bytes().first() {
        Some(b'\'') => single_quoted(&trimmed[1..], line_number).map(|value| (value, 0)),
        Some(b'"') => double_quoted(&trimmed[1..], following, line_number),
        _ => unquoted_value(rest, line_number).map(|value| (value, 0)),
    }
}

/// An unquoted value: the rest of the line, trimmed, with the trailing
/// comment rule applied.
///
/// A `#` is a comment only when whitespace comes before it *and* the value
/// before that whitespace has no whitespace of its own. `PORT=8080 # dev` is
/// a port and a comment; `KEY=v#3` is a three-character value; and
/// `PASSPHRASE=correct horse battery #4` refuses, because taking the comment
/// truncates a credential and not taking it stores a note.
fn unquoted_value(rest: &str, line_number: usize) -> Result<String, ParseError> {
    let comment_at = rest.char_indices().find(|&(index, c)| {
        c == '#'
            && rest[..index]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
    });
    let Some((index, _)) = comment_at else {
        return Ok(rest.trim().to_string());
    };
    let value = rest[..index].trim();
    if value.chars().any(char::is_whitespace) {
        return Err(ParseError {
            line: line_number,
            reason: ParseReason::AmbiguousComment,
        });
    }
    Ok(value.to_string())
}

/// A single-quoted value: literal, no escapes, closes on its own line.
fn single_quoted(after_quote: &str, line_number: usize) -> Result<String, ParseError> {
    let Some(end) = after_quote.find('\'') else {
        return Err(ParseError {
            line: line_number,
            reason: ParseReason::UnterminatedQuote,
        });
    };
    check_tail(&after_quote[end + 1..], line_number)?;
    Ok(after_quote[..end].to_string())
}

/// A double-quoted value: five escapes, and it may span lines.
fn double_quoted(
    after_quote: &str,
    following: &[&str],
    line_number: usize,
) -> Result<(String, usize), ParseError> {
    let mut value = String::new();
    let mut current = after_quote;
    let mut consumed = 0;
    loop {
        let mut chars = current.char_indices();
        while let Some((index, c)) = chars.next() {
            match c {
                '"' => {
                    check_tail(&current[index + 1..], line_number)?;
                    return Ok((value, consumed));
                }
                '\\' => {
                    let Some((_, escaped)) = chars.next() else {
                        return Err(ParseError {
                            line: line_number,
                            reason: ParseReason::UnterminatedQuote,
                        });
                    };
                    value.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        _ => {
                            return Err(ParseError {
                                line: line_number,
                                reason: ParseReason::BadEscape,
                            });
                        }
                    });
                }
                other => value.push(other),
            }
        }
        let Some(next) = following.get(consumed).copied() else {
            return Err(ParseError {
                line: line_number,
                reason: ParseReason::UnterminatedQuote,
            });
        };
        value.push('\n');
        consumed += 1;
        current = next;
    }
}

/// What may follow a closing quote: whitespace, then nothing or a comment.
fn check_tail(tail: &str, line_number: usize) -> Result<(), ParseError> {
    let tail = tail.trim();
    if tail.is_empty() || tail.starts_with('#') {
        return Ok(());
    }
    Err(ParseError {
        line: line_number,
        reason: ParseReason::TrailingText,
    })
}
```

Create `crates/shep-cli/src/commands/import/dotenv/mod.rs`:

```rust
//! `shep import env`: reading a `.env` into shep's stores.
//!
//! [`parse`] is the grammar and holds no shep concepts.

pub(crate) mod parse;
```

And add `pub(crate) mod dotenv;` beside `pub(crate) mod pm2;` in `import/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --lib --bins dotenv::parse
```

Expected: PASS, twelve tests.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli/src/commands/import
git commit
```

Subject: `feat(cli): parse a .env file, strictly`. The body explains the trailing-comment rule with all four of its cases, and why there is no interpolation.

---

## Task 3: Classification and shep's own limits

Turns parsed entries plus the operator's patterns into two write sets. Pure. Still no client, no files.

**Files:**
- Create: `crates/shep-cli/src/commands/import/dotenv/plan.rs`
- Modify: `crates/shep-cli/src/commands/import/dotenv/mod.rs` (declare it)
- Modify: `crates/shep-cli/Cargo.toml` (add `globset`)

**Interfaces:**
- Consumes: `parse::Entry` from Task 2.
- Produces:
  - `pub(crate) enum Class { Secret, Plain }`
  - `pub(crate) struct Planned { pub key: String, pub value: String, pub class: Class }`, redacted `Debug`.
  - `pub(crate) struct ImportPlan { pub entries: Vec<Planned>, pub unnamed: Vec<String> }`, redacted `Debug`.
  - `pub(crate) enum PlanError { BadPattern { pattern: String, message: String }, MatchedNothing { pattern: String }, NothingToImport, KeyNotStorable { key: String }, ValueTooLong { key: String, len: usize } }`, `Display`, `core::error::Error`.
  - `pub(crate) fn build(entries: Vec<Entry>, only: &[String], secret: &[String]) -> Result<ImportPlan, PlanError>`

Add to `crates/shep-cli/Cargo.toml` under `[dependencies]`, with the comment: `# Matching --secret and --only against a .env's keys, the same anchored-glob rule shep-core's sheep-name selector uses. Compiling three lines of matcher here beats a new public API on a published crate.`

```toml
globset.workspace = true
```

- [ ] **Step 1: Write the failing tests**

Create `crates/shep-cli/src/commands/import/dotenv/plan.rs` with its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::import::dotenv::parse;

    fn entries(text: &str) -> Vec<parse::Entry> {
        parse::parse(text).expect("this fixture parses")
    }

    const SAMPLE: &str = "NODE_ENV=production\nPORT=8080\nDB_PASSWORD=hunter2\nSTRIPE_TOKEN=sk_live\n";

    fn classes(plan: &ImportPlan) -> Vec<(&str, bool)> {
        plan.entries
            .iter()
            .map(|planned| (planned.key.as_str(), matches!(planned.class, Class::Secret)))
            .collect()
    }

    #[test]
    fn everything_is_plain_by_default() {
        let plan = build(entries(SAMPLE), &[], &[]).unwrap();
        assert_eq!(plan.entries.len(), 4);
        assert!(plan.entries.iter().all(|e| matches!(e.class, Class::Plain)));
    }

    #[test]
    fn an_exact_key_and_a_glob_both_classify() {
        let plan = build(
            entries(SAMPLE),
            &[],
            &["DB_PASSWORD".to_string(), "*_TOKEN".to_string()],
        )
        .unwrap();
        assert_eq!(
            classes(&plan),
            [
                ("NODE_ENV", false),
                ("PORT", false),
                ("DB_PASSWORD", true),
                ("STRIPE_TOKEN", true),
            ]
        );
    }

    #[test]
    fn only_filters_before_secret_classifies() {
        let plan = build(
            entries(SAMPLE),
            &["DB_*".to_string()],
            &["DB_PASSWORD".to_string()],
        )
        .unwrap();
        assert_eq!(classes(&plan), [("DB_PASSWORD", true)]);
    }

    #[test]
    fn a_glob_is_anchored() {
        let plan = build(entries("MY_DB_URL=x\nDB_URL=y\n"), &["DB_*".to_string()], &[]).unwrap();
        assert_eq!(classes(&plan), [("DB_URL", false)]);
    }

    #[test]
    fn a_pattern_matching_nothing_refuses() {
        let err = build(entries(SAMPLE), &[], &["DB_*NOPE".to_string()]).unwrap_err();
        assert!(matches!(err, PlanError::MatchedNothing { .. }));
        let err = build(entries(SAMPLE), &["absent".to_string()], &[]).unwrap_err();
        assert!(matches!(err, PlanError::MatchedNothing { .. }));
    }

    #[test]
    fn a_secretish_name_nobody_named_is_reported() {
        let plan = build(entries(SAMPLE), &[], &["DB_PASSWORD".to_string()]).unwrap();
        assert_eq!(plan.unnamed, ["STRIPE_TOKEN"]);
    }

    #[test]
    fn a_secret_key_shep_cannot_store_refuses() {
        let err = build(entries("A$B=1\n"), &[], &["*".to_string()]).unwrap_err();
        assert!(matches!(err, PlanError::KeyNotStorable { .. }));
    }

    #[test]
    fn a_secret_value_over_the_cap_refuses() {
        let long = "x".repeat(shep_core::secrets::MAX_VALUE_BYTES + 1);
        let err = build(
            entries(&format!("BIG={long}\n")),
            &[],
            &["BIG".to_string()],
        )
        .unwrap_err();
        assert!(matches!(err, PlanError::ValueTooLong { .. }));
    }

    #[test]
    fn an_empty_file_refuses() {
        let err = build(entries("# nothing here\n"), &[], &[]).unwrap_err();
        assert!(matches!(err, PlanError::NothingToImport));
    }

    /// IR-41.
    #[test]
    fn planned_and_plan_debug_do_not_leak() {
        let plan = build(entries("DB_PASSWORD=hunter2\n"), &[], &["DB_PASSWORD".to_string()])
            .unwrap();
        assert_eq!(
            format!("{:?}", plan.entries[0]),
            "Planned { key: \"DB_PASSWORD\", value: <7 bytes>, class: Secret }"
        );
        assert_eq!(
            format!("{plan:?}"),
            "ImportPlan { entries: <1 entries>, unnamed: [] }"
        );
    }

    /// No `PlanError` carries a value, so none of them can print one.
    #[test]
    fn no_plan_error_prints_a_value() {
        let long = "s3cret".repeat(1000);
        let err = build(entries(&format!("BIG={long}\n")), &[], &["BIG".to_string()])
            .unwrap_err();
        assert!(!err.to_string().contains("s3cret"), "{err}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins dotenv::plan
```

Expected: FAIL, `build` not found.

- [ ] **Step 3: Write the planner**

Above the tests in the same file:

```rust
//! What each parsed key is, and whether shep can hold it.
//!
//! `--only` filters and `--secret` classifies what survives. Both take the
//! same argument grammar the sheep-name selector takes: an anchored glob,
//! which for a pattern with no metacharacter in it is an exact match.
//!
//! A pattern matching nothing refuses the whole import. For `--secret` that
//! is a leak rule rather than a tidiness one: a glob that misses the keys it
//! was aimed at would otherwise write credentials into the override store in
//! the clear and exit 0.

use core::fmt;

use globset::Glob;
use shep_core::secrets::{self, MAX_VALUE_BYTES};

use super::parse::Entry;

/// Substrings that make a name look like a credential.
///
/// Closed and short on purpose, and it decides nothing: a match that no
/// `--secret` pattern claimed is named on stderr and imported anyway. Do
/// not grow it by guessing, which is the rule the pm2 importer's own key
/// lists carry for the same reason.
const SECRETISH: &[&str] = &[
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "KEY",
    "DSN",
    "CREDENTIAL",
    "PRIVATE",
];

/// Which store a key is bound for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// The value goes to `secrets.json`; the sheep's env gets a reference.
    Secret,
    /// The value goes to the sheep's env as it stands.
    Plain,
}

/// One key, its value, and where it is going.
///
/// `Debug` prints the value's length and never the value (IR-41).
pub(crate) struct Planned {
    /// The key.
    pub key: String,
    /// The value.
    pub value: String,
    /// Which store.
    pub class: Class,
}

impl fmt::Debug for Planned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Planned")
            .field("key", &self.key)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .field("class", &self.class)
            .finish()
    }
}

/// Everything the import is about to write, and what it wants to warn about.
///
/// `Debug` counts the entries rather than listing them: each one holds a
/// value (IR-41). `unnamed` is keys only and prints in full.
pub(crate) struct ImportPlan {
    /// Every key that survived `--only`, in the file's own order.
    pub entries: Vec<Planned>,
    /// Keys whose names look like credentials that no `--secret` claimed.
    pub unnamed: Vec<String>,
}

impl fmt::Debug for ImportPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportPlan")
            .field(
                "entries",
                &format_args!("<{} entries>", self.entries.len()),
            )
            .field("unnamed", &self.unnamed)
            .finish()
    }
}

/// Why a plan could not be built.
///
/// `Debug` is derived: no variant carries a value.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PlanError {
    /// A pattern globset would not compile.
    BadPattern {
        /// The pattern as typed.
        pattern: String,
        /// globset's own message.
        message: String,
    },
    /// A pattern that matched none of the file's keys.
    MatchedNothing {
        /// The pattern as typed.
        pattern: String,
    },
    /// The file held no keys, or `--only` left none.
    NothingToImport,
    /// A key classified secret that the secret store's grammar refuses.
    KeyNotStorable {
        /// The key.
        key: String,
    },
    /// A value classified secret that is over the store's cap.
    ValueTooLong {
        /// The key.
        key: String,
        /// Its length in bytes.
        len: usize,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPattern { pattern, message } => {
                write!(f, "`{pattern}` is not a valid pattern: {message}")
            }
            Self::MatchedNothing { pattern } => write!(
                f,
                "`{pattern}` matched no key in the file; nothing was imported"
            ),
            Self::NothingToImport => f.write_str("the file holds no keys to import"),
            Self::KeyNotStorable { key } => write!(
                f,
                "`{key}` cannot be a secret: a key is letters, digits, `.`, `_` and `-`, \
                 at most {} bytes, and does not start with a dot",
                secrets::MAX_KEY_BYTES
            ),
            Self::ValueTooLong { key, len } => write!(
                f,
                "`{key}` is {len} bytes, over the {MAX_VALUE_BYTES}-byte limit for a secret"
            ),
        }
    }
}

impl core::error::Error for PlanError {}

/// Filters, classifies, and checks the store's limits.
///
/// # Errors
/// [`PlanError`], for a pattern that will not compile or matches nothing, a
/// file with nothing left to import, or a secret shep cannot hold. No error
/// carries a value.
pub(crate) fn build(
    entries: Vec<Entry>,
    only: &[String],
    secret: &[String],
) -> Result<ImportPlan, PlanError> {
    let kept: Vec<Entry> = if only.is_empty() {
        entries
    } else {
        let matched = select(&entries, only)?;
        entries
            .into_iter()
            .filter(|entry| matched.contains(&entry.key))
            .collect()
    };
    if kept.is_empty() {
        return Err(PlanError::NothingToImport);
    }

    let secrets_named = select(&kept, secret)?;

    let mut planned = Vec::with_capacity(kept.len());
    let mut unnamed = Vec::new();
    for entry in kept {
        let class = if secrets_named.contains(&entry.key) {
            if !secrets::is_name(&entry.key) {
                return Err(PlanError::KeyNotStorable { key: entry.key });
            }
            if entry.value.len() > MAX_VALUE_BYTES {
                return Err(PlanError::ValueTooLong {
                    key: entry.key,
                    len: entry.value.len(),
                });
            }
            Class::Secret
        } else {
            if looks_secret(&entry.key) {
                unnamed.push(entry.key.clone());
            }
            Class::Plain
        };
        planned.push(Planned {
            key: entry.key,
            value: entry.value,
            class,
        });
    }
    Ok(ImportPlan {
        entries: planned,
        unnamed,
    })
}

/// The keys `patterns` match, refusing any pattern that matches none.
fn select(entries: &[Entry], patterns: &[String]) -> Result<Vec<String>, PlanError> {
    let mut matched = Vec::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|err| PlanError::BadPattern {
                pattern: pattern.clone(),
                message: err.to_string(),
            })?
            .compile_matcher();
        let hits: Vec<String> = entries
            .iter()
            .filter(|entry| glob.is_match(&entry.key))
            .map(|entry| entry.key.clone())
            .collect();
        if hits.is_empty() {
            return Err(PlanError::MatchedNothing {
                pattern: pattern.clone(),
            });
        }
        matched.extend(hits);
    }
    Ok(matched)
}

/// Whether a name reads like a credential.
fn looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SECRETISH.iter().any(|needle| upper.contains(needle))
}
```

Declare it in `dotenv/mod.rs`: `pub(crate) mod plan;`.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --lib --bins dotenv::plan
```

Expected: PASS, eleven tests.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
```
```bash
cargo test --workspace --all-features
```

- [ ] **Step 6: Commit**

```bash
git add crates/shep-cli
git commit
```

Subject: `feat(cli): classify a .env's keys against --only and --secret`.

---

## Task 4: `Request::SetSheepEnvBatch` on the wire

Additive. No version moves.

**Files:**
- Modify: `crates/shep-core/src/protocol/request.rs` (`Request`, after `SetSheepEnv` around line 304; `Response`, near `SheepEnvSet`)

**Interfaces:**
- Consumes: `EnvValue` (already there).
- Produces:
  - `Request::SetSheepEnvBatch { name: String, entries: BTreeMap<String, EnvValue>, force: bool, dry_run: bool }`
  - `Response::SheepEnvBatch { name: String, set: Vec<String>, unchanged: Vec<String>, collisions: Vec<String> }`

**Correction to the spec.** Decision 14 asks for a manual `Debug` on `Request::SetSheepEnvBatch`. `Debug` is per type, not per variant, and `Request` derives it, which is exactly why `EnvValue` exists (`request.rs:1194`). So the variant carries no new `Debug` impl; it relies on `EnvValue`'s, pinned by the exact-string test in Step 1. Do not hand-write a `Debug` for `Request`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-core/src/protocol/request.rs`, inside its existing `mod tests`:

```rust
/// IR-41. `EnvValue` is what keeps the derive on `Request` safe, and this
/// pins that the batch variant actually uses it.
#[test]
fn set_sheep_env_batch_debug_does_not_leak() {
    let request = Request::SetSheepEnvBatch {
        name: "web".to_string(),
        entries: BTreeMap::from([(
            "DB_PASSWORD".to_string(),
            EnvValue::from("hunter2".to_string()),
        )]),
        force: false,
        dry_run: true,
    };
    assert_eq!(
        format!("{request:?}"),
        "SetSheepEnvBatch { name: \"web\", entries: {\"DB_PASSWORD\": EnvValue(<7 bytes>)}, \
         force: false, dry_run: true }"
    );
}

/// The wire shape, pinned the way every other variant's is.
#[test]
fn set_sheep_env_batch_wire_v8() {
    let request = Request::SetSheepEnvBatch {
        name: "web".to_string(),
        entries: BTreeMap::from([("A".to_string(), EnvValue::from("1".to_string()))]),
        force: true,
        dry_run: false,
    };
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"set_sheep_env_batch","name":"web","entries":{"A":"1"},"force":true,"dry_run":false}"#
    );
    assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
}

/// The reply carries key names and never a value.
#[test]
fn sheep_env_batch_response_wire_v8() {
    let response = Response::SheepEnvBatch {
        name: "web".to_string(),
        set: vec!["A".to_string()],
        unchanged: vec!["B".to_string()],
        collisions: Vec::new(),
    };
    let json = serde_json::to_string(&response).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"sheep_env_batch","data":{"name":"web","set":["A"],"unchanged":["B"],"collisions":[]}}"#
    );
    assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), response);
}

/// Additive, so the version does not move. Guards against a reflexive bump.
#[test]
fn the_batch_variant_did_not_move_the_version() {
    assert_eq!(super::super::PROTOCOL_VERSION, 8);
    assert_eq!(super::super::MIN_SUPPORTED, 8);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins set_sheep_env_batch
```

Expected: FAIL, no variant `SetSheepEnvBatch`.

- [ ] **Step 3: Add the variants**

In `Request`, immediately after `SetSheepEnv`:

```rust
    /// Sets several env keys on one sheep in a single write.
    ///
    /// [`Self::SetSheepEnv`]'s doc says a request taking a map would need
    /// per-key reporting back "for no caller that wants it". `shep import
    /// env` is that caller: it writes twenty keys at once and has to refuse
    /// the whole set rather than leave eleven of them applied.
    ///
    /// The daemon applies this as one read-modify-write of the override
    /// store, so every key lands or none does. No removal arm: a pane
    /// deletes rows one at a time through [`Self::SetSheepEnv`], and an
    /// import never removes anything.
    ///
    /// A key already holding a different value is a collision. Without
    /// `force` any collision refuses the whole request and writes nothing;
    /// with it, the collisions are overwritten and named in the reply as
    /// well as counted in `set`. A key already holding the same value is
    /// `unchanged`, never a collision, so re-running an unchanged import
    /// needs no flag.
    ///
    /// `dry_run` computes the three lists and writes nothing.
    ///
    /// Parks for the next spawn, exactly as [`Self::SetSheepEnv`] does.
    ///
    /// Answers [`Response::SheepEnvBatch`], or
    /// [`RpcErrorCode::NotFound`] when no sheep has that name.
    SetSheepEnvBatch {
        /// The sheep's name.
        name: String,
        /// The keys and their values.
        ///
        /// [`EnvValue`], not `String`, for [`Self::SetSheepEnv`]'s reason:
        /// `Request` derives `Debug` and this map is the densest run of
        /// secrets on the wire (IR-41).
        entries: BTreeMap<String, EnvValue>,
        /// Overwrite colliding keys instead of refusing.
        force: bool,
        /// Report what would happen and write nothing.
        dry_run: bool,
    },
```

In `Response`, beside `SheepEnvSet`:

```rust
    /// What a [`Request::SetSheepEnvBatch`] did, or would have done.
    ///
    /// Key names only. `set` is what was written, `unchanged` what already
    /// held the same value, `collisions` what held a different one. A
    /// forced request reports a collision in both `set` and `collisions`;
    /// an unforced one that collides reports an empty `set` and wrote
    /// nothing.
    SheepEnvBatch {
        /// The sheep's name.
        name: String,
        /// Keys written.
        set: Vec<String>,
        /// Keys that already held this value.
        unchanged: Vec<String>,
        /// Keys that held a different value.
        collisions: Vec<String>,
    },
```

Add `use std::collections::BTreeMap;` to the test module if it is not already imported there.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --lib --bins set_sheep_env_batch
```

Expected: PASS. `cargo build --workspace` will keep building after this commit: `Request` is `#[non_exhaustive]` and `crates/shep-daemon/src/rpc.rs`'s dispatch already ends in a wildcard arm, so the new variant falls through to it silently rather than refusing to compile. That silent fallthrough is exactly why `rpc.rs`'s `every_new_variant_reaches_an_arm_and_not_the_wildcard` test exists: it is a hand-written completeness list, and `SetSheepEnvBatch` must be added to it in this commit or the one that lands the daemon arm, whichever comes first, or the variant is silently refused at runtime with no compiler or test signal.

This task and Task 5 still land in that order, because Task 5 is what makes the new request do anything; the workspace builds the whole time, it just answers `SetSheepEnvBatch` with the wildcard's "not implemented" error until Task 5's arm lands. Run the gate after Task 5.

- [ ] **Step 5: Commit**

```bash
git add crates/shep-core/src/protocol/request.rs
git commit --no-verify
```

Subject: `feat(core): add a batched sheep-env request`. The body says the workspace does not build until the daemon arm lands, names Task 5's commit as the one that closes it, and records that `PROTOCOL_VERSION` deliberately stays 8.

---

## Task 5: The daemon applies the batch

**Files:**
- Modify: `crates/shep-daemon/src/supervisor.rs` (`Command` around line 317, the handle method near `set_sheep_env`, `handle_set_sheep_env_batch` beside `handle_set_sheep_env` around line 4534, the `Command` dispatch around line 2630)
- Modify: `crates/shep-daemon/src/rpc.rs` (a new arm beside `Request::SetSheepEnv`, around line 705)

**Interfaces:**
- Consumes: `Request::SetSheepEnvBatch` and `Response::SheepEnvBatch` from Task 4.
- Produces: `pub struct EnvBatch { pub app: Option<ResolvedApp>, pub set: Vec<String>, pub unchanged: Vec<String>, pub collisions: Vec<String> }` and `Supervisor::set_sheep_env_batch(name: String, entries: BTreeMap<String, String>, force: bool, dry_run: bool) -> Result<Option<EnvBatch>, SupervisorError>`. `app` is `Some` only when something was written.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-daemon/src/supervisor.rs`'s test module, beside the existing `set_sheep_env` tests:

```rust
#[tokio::test]
async fn a_batch_writes_every_key_under_one_lock() {
    let fixture = env_fixture().await;
    let batch = fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A", "B"]);
    assert!(batch.collisions.is_empty());
    let record = overrides::get(&fixture.paths.overrides, "web").unwrap().unwrap();
    let env = record.fields["env"].as_object().unwrap();
    assert_eq!(env["A"], "1");
    assert_eq!(env["B"], "2");
}

#[tokio::test]
async fn an_identical_value_is_unchanged_rather_than_a_collision() {
    let fixture = env_fixture().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    fixture
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries.clone(), false, false)
        .await
        .unwrap();
    let batch = fixture
        .supervisor
        .set_sheep_env_batch("web".to_string(), entries, false, false)
        .await
        .unwrap()
        .expect("web exists");
    assert!(batch.set.is_empty());
    assert_eq!(batch.unchanged, ["A"]);
    assert!(batch.collisions.is_empty());
}

#[tokio::test]
async fn a_collision_without_force_writes_nothing_at_all() {
    let fixture = env_fixture().await;
    fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "9".to_string()),
            ]),
            false,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.collisions, ["A"]);
    assert!(batch.set.is_empty());
    assert!(batch.app.is_none());
    let record = overrides::get(&fixture.paths.overrides, "web").unwrap().unwrap();
    let env = record.fields["env"].as_object().unwrap();
    assert_eq!(env["A"], "1", "the colliding key kept its value");
    assert!(!env.contains_key("B"), "the clean key was not written either");
}

#[tokio::test]
async fn force_overwrites_and_reports_the_collision_in_both_lists() {
    let fixture = env_fixture().await;
    fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            false,
        )
        .await
        .unwrap();
    let batch = fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "2".to_string())]),
            true,
            false,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert_eq!(batch.collisions, ["A"]);
    let record = overrides::get(&fixture.paths.overrides, "web").unwrap().unwrap();
    assert_eq!(record.fields["env"].as_object().unwrap()["A"], "2");
}

#[tokio::test]
async fn a_dry_run_answers_and_writes_nothing() {
    let fixture = env_fixture().await;
    let batch = fixture
        .supervisor
        .set_sheep_env_batch(
            "web".to_string(),
            BTreeMap::from([("A".to_string(), "1".to_string())]),
            false,
            true,
        )
        .await
        .unwrap()
        .expect("web exists");
    assert_eq!(batch.set, ["A"]);
    assert!(batch.app.is_none());
    assert!(
        overrides::get(&fixture.paths.overrides, "web").unwrap().is_none(),
        "a dry run left a store behind"
    );
}

#[tokio::test]
async fn a_batch_refuses_a_dog_and_an_unknown_name() {
    let fixture = env_fixture().await;
    let entries = BTreeMap::from([("A".to_string(), "1".to_string())]);
    assert!(
        fixture
            .supervisor
            .set_sheep_env_batch("absent".to_string(), entries.clone(), false, false)
            .await
            .unwrap()
            .is_none()
    );
    let err = fixture
        .supervisor
        .set_sheep_env_batch(fixture.dog_name.clone(), entries, false, false)
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::IsADog(_)));
}
```

Reuse whatever fixture the existing `set_sheep_env` tests use. If they build their supervisor inline rather than through a helper, follow that shape rather than adding one; `env_fixture` above is a placeholder for the fixture already in the file, and the dog case needs a fixture that registers one.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --lib --bins -- a_batch_writes_every_key_under_one_lock an_identical_value_is_unchanged_rather_than_a_collision a_collision_without_force_writes_nothing_at_all force_overwrites_and_reports_the_collision_in_both_lists a_dry_run_answers_and_writes_nothing a_batch_refuses_a_dog_and_an_unknown_name
```

Expected: FAIL, no method `set_sheep_env_batch`.

- [ ] **Step 3: Implement it**

`Command`, beside `SetSheepEnv`:

```rust
    /// Several env keys on one sheep, applied as one write.
    SetSheepEnvBatch {
        /// The sheep's name.
        name: String,
        /// The keys and their values.
        entries: BTreeMap<String, String>,
        /// Overwrite colliding keys.
        force: bool,
        /// Compute and write nothing.
        dry_run: bool,
        /// Where the answer goes.
        reply: oneshot::Sender<Result<Option<EnvBatch>, SupervisorError>>,
    },
```

Match the exact `reply` channel type the neighbouring `SetSheepEnv` variant uses; the line above is its shape, not necessarily its spelling.

The result type, beside `FieldSet`:

```rust
/// What a [`Command::SetSheepEnvBatch`] did.
///
/// `Debug` is derived: every field is a key name, and `ResolvedApp` has its
/// own decision already.
#[derive(Debug)]
pub struct EnvBatch {
    /// The parked config, for the caller to record. `None` when nothing was
    /// written, which is a dry run or an unforced collision.
    pub app: Option<ResolvedApp>,
    /// Keys written.
    pub set: Vec<String>,
    /// Keys that already held this value.
    pub unchanged: Vec<String>,
    /// Keys that held a different value.
    pub collisions: Vec<String>,
}
```

The handler, beside `handle_set_sheep_env`:

```rust
    /// Records several env keys on `name` as operator overrides in one
    /// write, and parks them for the next spawn.
    ///
    /// `Ok(None)` when no sheep has that name.
    ///
    /// # Why this is not a loop over [`Self::handle_set_sheep_env`]
    ///
    /// That function writes the store once per key. Twenty keys would be
    /// twenty read-modify-writes, and a failure at the eleventh would leave
    /// half an import applied with no record of which half. This validates
    /// every key against the intended config first, then writes once.
    ///
    /// # Collisions
    ///
    /// A key already holding a different value in the intended config
    /// collides. Without `force`, one collision refuses the whole batch and
    /// nothing is written. A key holding the same value is `unchanged` and
    /// is not rewritten, so a repeated identical batch is a no-op.
    ///
    /// # Errors
    ///
    /// - [`SupervisorError::IsADog`] - the name is a dog's. Raised before
    ///   the store is read, for [`Self::handle_set_sheep_env`]'s reason.
    /// - [`SupervisorError::InvalidEnv`] - the resulting config is one
    ///   `normalize` refuses. Nothing was written.
    /// - [`SupervisorError::Overrides`] - the store could not be read or
    ///   written. Nothing was parked.
    fn handle_set_sheep_env_batch(
        &mut self,
        name: &str,
        entries: &BTreeMap<String, String>,
        force: bool,
        dry_run: bool,
    ) -> Result<Option<EnvBatch>, SupervisorError> {
        let Some(id) = self.representative_id(name) else {
            return Ok(None);
        };
        // Before the store is read, for `handle_set_sheep_env`'s reason: a
        // dog runs at the daemon's own trust level.
        if self
            .sheep
            .get(&id)
            .is_some_and(|slot| slot.entry.dog.is_some())
        {
            return Err(SupervisorError::IsADog(dog_config_refusal(name)));
        }
        let Some(mut intended) = self.intended_spec(id).map(|spec| spec.config().clone()) else {
            return Ok(None);
        };

        let mut set = Vec::new();
        let mut unchanged = Vec::new();
        let mut collisions = Vec::new();
        for (key, value) in entries {
            match intended.env.get(key) {
                Some(current) if current == value => unchanged.push(key.clone()),
                Some(_) => {
                    collisions.push(key.clone());
                    if force {
                        set.push(key.clone());
                    }
                }
                None => set.push(key.clone()),
            }
        }

        let refused = !collisions.is_empty() && !force;
        if refused || dry_run {
            return Ok(Some(EnvBatch {
                app: None,
                set: if refused { Vec::new() } else { set },
                unchanged,
                collisions,
            }));
        }

        for key in &set {
            intended
                .env
                .insert(key.clone(), entries[key].clone());
        }
        let parked =
            normalize(intended).map_err(|err| SupervisorError::InvalidEnv(err.to_string()))?;

        let mut record = overrides::get(&self.paths.overrides, name)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?
            .unwrap_or_default();
        // The same flat object `merge_declared` reads, and the same refusal
        // `handle_set_sheep_env` makes when a later shep wrote something
        // else there.
        let env = record
            .fields
            .entry("env".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let Some(map) = env.as_object_mut() else {
            return Err(SupervisorError::Overrides(format!(
                "{name}'s stored `env` override is not an object"
            )));
        };
        for key in &set {
            map.insert(
                key.clone(),
                serde_json::Value::String(entries[key].clone()),
            );
        }
        // No tombstone handling and no `emptied` branch: this door only
        // ever inserts, so the map cannot come out empty and no key can
        // stop being held.
        let overridden: Vec<String> = record.fields.keys().cloned().collect();
        let changes = BTreeMap::from([(name.to_string(), Some(record))]);
        overrides::update(&self.paths.overrides, &changes)
            .map_err(|err| SupervisorError::Overrides(err.to_string()))?;

        for id in self.ids_of_name(name) {
            let Some(slot) = self.sheep.get_mut(&id) else {
                continue;
            };
            slot.entry.pending = Some(parked.clone());
            slot.entry.overridden.clone_from(&overridden);
        }
        Ok(Some(EnvBatch {
            app: Some(parked),
            set,
            unchanged,
            collisions,
        }))
    }
```

Add the `Command::SetSheepEnvBatch` dispatch arm beside `Command::SetSheepEnv`'s, and the public `set_sheep_env_batch` method on the handle beside `set_sheep_env`, both copying the neighbour's shape exactly.

The rpc arm, beside `Request::SetSheepEnv`:

```rust
        Request::SetSheepEnvBatch {
            name,
            entries,
            force,
            dry_run,
        } => {
            let values: BTreeMap<String, String> = entries
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().to_string()))
                .collect();
            match ctx
                .supervisor
                .set_sheep_env_batch(name.clone(), values, force, dry_run)
                .await
            {
                // Recorded for `SetSheepEnv`'s reason: the muster roll is
                // written from the registry and nothing on the restore path
                // reads the override store. `app` is `None` for a dry run
                // and for a refused collision, and neither wrote anything
                // to record.
                Ok(Some(batch)) => {
                    if let Some(app) = batch.app {
                        ctx.registry.record(&[app]);
                    }
                    reply(Ok(Response::SheepEnvBatch {
                        name,
                        set: batch.set,
                        unchanged: batch.unchanged,
                        collisions: batch.collisions,
                    }))
                }
                Ok(None) => reply(Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: format!("no sheep named {name}"),
                    daemon_version: None,
                })),
                Err(err) => reply(Err(rpc_error(&err))),
            }
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --lib --bins -- a_batch_writes_every_key_under_one_lock an_identical_value_is_unchanged_rather_than_a_collision a_collision_without_force_writes_nothing_at_all force_overwrites_and_reports_the_collision_in_both_lists a_dry_run_answers_and_writes_nothing a_batch_refuses_a_dog_and_an_unknown_name
```

Expected: PASS.

- [ ] **Step 5: Run the full gate**

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

- [ ] **Step 6: Commit**

```bash
git add crates/shep-daemon
git commit
```

Subject: `feat(daemon): apply a batched sheep-env request under one lock`.

---

## Task 6: The verb

**Files:**
- Create: nothing new; fill in `crates/shep-cli/src/commands/import/dotenv/mod.rs`
- Modify: `crates/shep-cli/src/cli.rs` (`ImportCommand::Env`, `ImportEnvArgs`)
- Modify: `crates/shep-cli/src/commands/import/mod.rs` (`import` becomes async and takes a client for `env`)
- Modify: `crates/shep-cli/src/lib.rs` (the `Commands::Import` arm moves into the connecting group for `env`)
- Modify: `crates/shep-cli/src/output/rows.rs` (`ImportEnvRow`, `ImportEnvRows`)
- Test: `crates/shep-cli/tests/cli_e2e.rs`

**Interfaces:**
- Consumes: `dotenv::parse::parse`, `dotenv::plan::build`, `Request::SetSheepEnvBatch`, `Response::SheepEnvBatch`, `secrets::set`, `secrets::get`, `Request::SheepConfig`.
- Produces: `commands::import::dotenv::import_env(client, streams, paths, args) -> ExitCode`.

- [ ] **Step 1: Write the failing tests**

In `crates/shep-cli/tests/cli_e2e.rs`, following whichever existing case starts a daemon and a sheep (copy its `DaemonGuard` and `shep(home)` usage exactly):

```rust
/// The whole verb, end to end: two plain keys into the sheep's env, one
/// secret into the store with a reference left behind.
#[test]
fn import_env_splits_a_dotenv_between_the_two_stores() {
    // start a shepherd and one sheep named `web`, per the neighbouring cases
    // ...
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nPORT=8080\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("hunter2") && !combined.contains("8080"),
        "a value reached an output stream: {combined}"
    );

    let described = shep(home.path())
        .args(["describe", "web", "--format", "json"])
        .output()
        .unwrap();
    let described = String::from_utf8_lossy(&described.stdout);
    assert!(
        described.contains("{{secret:DB_PASSWORD}}") || described.contains("DB_PASSWORD"),
        "the reference did not reach the sheep: {described}"
    );
}

/// Re-running an unchanged file is a no-op. Changing one value refuses the
/// whole import until `--force`.
#[test]
fn import_env_refuses_a_changed_value_without_force() {
    // same fixture as above, with the first import already done
    // ...
    std::fs::write(home.path().join("app.env"), "PORT=9090\nNODE_ENV=production\n").unwrap();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("PORT"), "the colliding key was not named: {err}");
    assert!(!err.contains("9090"), "the value reached stderr: {err}");
}

/// A pattern that matches nothing refuses before anything is written.
#[test]
fn import_env_refuses_a_pattern_that_matches_nothing() {
    // ...
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "ABSENT_*",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

/// `--dry-run` writes to neither store.
#[test]
fn import_env_dry_run_writes_nothing() {
    // ...
    let before = std::fs::read_to_string(home.path().join("secrets.json")).ok();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(home.path().join("secrets.json")).ok(),
        before
    );
}

/// An unknown sheep is a `NotFound`, and it is reported before either store
/// is touched.
#[test]
fn import_env_refuses_an_unknown_app() {
    // ...
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "absent",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --workspace --all-features --test cli_e2e import_env
```

Expected: FAIL, unrecognized subcommand `env`.

- [ ] **Step 3: Add the arguments**

In `cli.rs`, add to `ImportCommand`:

```rust
    /// Read a `.env` into the secret store and one sheep's own env.
    ///
    /// Every key the file holds goes to the named sheep's env, where it
    /// reaches the app at its next spawn. A key named by `--secret` has its
    /// value stored in `$SHEP_HOME/secrets.json` instead, and the sheep's
    /// env gets `{{secret:KEY}}`, so the value never reaches `flock.json`
    /// or the handover blob.
    ///
    /// A key that is not marked secret is stored in the clear, and is
    /// copied into both of those snapshots along with the rest of the
    /// sheep's config.
    ///
    /// The sheep has to exist already: this records an operator override,
    /// which is per sheep. Run `shep start` first.
    ///
    /// Any collision, any pattern that matches nothing, and any line the
    /// grammar does not accept refuses the whole import and writes nothing.
    Env(ImportEnvArgs),
```

```rust
/// Arguments to `shep import env`.
///
/// `Debug` is derived: every field is a path, a name or a pattern. The
/// values live in the file this names, never in the arguments, which is
/// also why there is no `--stdin`.
#[derive(Debug, clap::Args)]
pub struct ImportEnvArgs {
    /// The `.env` to read
    pub file: PathBuf,
    /// The sheep whose env these keys belong to
    #[arg(long)]
    pub app: String,
    /// Store this key's value as a secret, and reference it from the env.
    ///
    /// An exact key, or a glob when it holds a metacharacter, the same rule
    /// a sheep-name selector takes. Repeatable. A pattern matching no key
    /// in the file refuses the import.
    #[arg(long)]
    pub secret: Vec<String>,
    /// Import only these keys; the default is all of them.
    ///
    /// Same grammar as `--secret`, and the same refusal.
    #[arg(long)]
    pub only: Vec<String>,
    /// Which environment's slot the secrets go in.
    ///
    /// The default is the sheep's own environment, which is its
    /// `environment` field, or `[daemon] environment` when it has none.
    /// Never the `all` slot: every environment reads that one.
    #[arg(long)]
    pub env: Option<String>,
    /// Print what would be written, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite keys that already hold a different value
    #[arg(long)]
    pub force: bool,
}
```

- [ ] **Step 4: Add the output rows**

In `crates/shep-cli/src/output/rows.rs`, following `ImportRow`'s shape:

```rust
/// One key `shep import env` wrote, or would write.
///
/// `Debug` is derived and stays that way only because there is no value
/// here: `bytes` is a length. The row exists in this shape precisely so
/// that neither format can print a value (IR-41).
#[derive(Debug, Serialize)]
pub struct ImportEnvRow {
    /// The key.
    pub key: String,
    /// `secret` or `env`.
    pub store: String,
    /// The environment slot, for a secret. `-` for an env key.
    pub slot: String,
    /// The value's length in bytes.
    pub bytes: usize,
}

/// `shep import env`: one row per key.
///
/// `transparent` so the JSON is a plain array.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ImportEnvRows(pub Vec<ImportEnvRow>);
```

Implement `Render` for `ImportEnvRows` with headers `["KEY", "STORE", "SLOT", "BYTES"]`, `json_key_for` mapping each to its lowercase name and panicking with `#[track_caller]` on anything else, `JSON_ONLY: &[]`, and `PRIORITIES: &[0, 0, 1, 2]`. Paint `STORE` as `Role::Butter` when the cell is `secret`, following `ImportRows::rows_for`.

- [ ] **Step 5: Write the verb**

Fill in `crates/shep-cli/src/commands/import/dotenv/mod.rs`. The order below is the design's decision 13 and the reason for it is in the doc comment.

```rust
//! `shep import env`: reading a `.env` into shep's stores.
//!
//! [`parse`] is the grammar. [`plan`] decides which store each key is bound
//! for and whether shep can hold it. This module does the I/O, in one order:
//!
//! 1. read and parse, then plan. Nothing written.
//! 2. `Request::SheepConfig`: the sheep exists, is not a dog, and resolves
//!    to an environment.
//! 3. `SetSheepEnvBatch` with `dry_run`, so the daemon names env collisions
//!    against the values it holds and this process never sees them.
//! 4. secret-store collisions, decided here, since this process holds those
//!    values.
//! 5. any collision without `--force`: name them all and exit, writing
//!    nothing.
//! 6. write `secrets.json`, then send the batch for real.
//!
//! Secrets first and the batch second on purpose. A failure at the last step
//! leaves values in the store that nothing references, which are inert, and
//! the re-run is clean because an identical value is not a collision. The
//! reverse order leaves `{{secret:KEY}}` pointing at nothing and the re-run
//! then needs `--force`.

pub(crate) mod parse;
pub(crate) mod plan;
```

Then `import_env`, whose skeleton is:

```rust
pub async fn import_env(
    client: &Client,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &ImportEnvArgs,
) -> ExitCode
```

with these decisions baked in, each of which the tests above pin:

- A read failure on `args.file` is `ExitCode::Usage` with the path and the OS error. A `ParseError` is `ExitCode::InvalidConfig`: the file on disk is the problem, the same split `commands::secret::exit_code_for` makes. A `PlanError` is `ExitCode::Usage`: the operator typed the pattern.
- The environment is `args.env`, else the `SheepConfigView`'s `config.environment`, else `daemon_config(paths).daemon.environment` through `commands::secret::daemon_config`, which is already `pub(crate)` for exactly this kind of reuse.
- A `RpcErrorCode::NotFound` from either request exits `ExitCode::NotFound`.
- Secret collisions come from `secrets::get(&paths.secrets, key, &environment)`: `Some(existing)` equal to the planned value is unchanged, `Some(other)` is a collision, `None` is a write.
- Every collision from both stores is named on one stderr line each, with its store, and the exit is `ExitCode::Usage`. No value.
- `plan.unnamed` is written to stderr through `streams.aside("secretish", ...)` before any write, one line naming every key and saying they were stored in the clear.
- The last line before the rows is an aside saying the change parks for the next spawn and that `shep reload <app>` promotes it.
- `--dry-run` stops after step 5 and emits the rows.

- [ ] **Step 6: Wire the dispatch**

`import::import` becomes:

```rust
pub async fn import(
    client: Option<&Client>,
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    args: &ImportArgs,
) -> ExitCode
```

Prefer instead splitting the dispatch in `lib.rs`, which keeps `pm2` free of a `Client` it never uses:

```rust
        // pm2 reads a file and writes a file; `env` writes an operator
        // override, which is the daemon's own store.
        Commands::Import(ref args) => match &args.command {
            ImportCommand::Pm2(args) => import::pm2::import(&mut streams, args),
            ImportCommand::Env(args) => match connect_client(&mut streams, &paths, guard).await {
                Ok(client) => import::dotenv::import_env(&client, &mut streams, &paths, args).await,
                Err(code) => code,
            },
        },
```

and delete `import::import`, since nothing calls it any more. Update the comment above the arm, which currently says the verb starts nothing and asks the socket nothing.

- [ ] **Step 7: Run the tests to verify they pass**

```bash
cargo test --workspace --all-features --test cli_e2e import_env
```

Expected: PASS, five cases.

- [ ] **Step 8: Run the full gate**

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

- [ ] **Step 9: Commit**

```bash
git add crates/shep-cli
git commit
```

Subject: `feat(cli): import a .env into the secret store and a sheep's env`.

---

## Task 7: The docs

**Files:**
- Modify: `web/src/data/cli-reference.generated.txt` (regenerated, never hand-edited)
- Modify: `web/src/pages/docs/from-pm2.astro`, `first-flockfile.astro` (around line 303), `startup.astro` (around line 239), `secrets.astro`, `overrides.astro`
- Modify: `web/src/components/landing/Features.astro` (around line 59)
- Modify: `crates/shep-cli/README.md` (around line 37)

**Interfaces:**
- Consumes: the finished verb from Task 6.
- Produces: nothing code depends on.

- [ ] **Step 1: Regenerate the reference**

```bash
cargo build --release
```
```bash
./web/scripts/generate-cli-reference.sh
```
```bash
git diff --stat web/src/data/cli-reference.generated.txt
```

The diff must show `import pm2` and `import env`. A stale copy fails no build, which is why this step is first.

- [ ] **Step 2: Rename every `shep import` that means the pm2 dump**

```bash
grep -rn "shep import" web/src crates/shep-cli/README.md | grep -v cli-reference.generated
```

Every hit is the pm2 verb and becomes `shep import pm2`. `from-pm2.astro` carries about a dozen, including a `VerbSignature` whose `usage` prop changes to `shep import pm2 [OPTIONS]` and two `CodeBlock` labels. `Features.astro:59`'s title becomes `shep import pm2 brings your dump.pm2 over`.

- [ ] **Step 3: Write the new prose**

In `secrets.astro`, a section on the import: the command, what `--secret` does to a key, that the sheep must exist, that the default environment is the sheep's own and never `all`, and that a refusal writes nothing.

In `overrides.astro`, one paragraph: `shep import env` is a third door into the override store, beside a Flockfile load and a lookout pane, and the keys it writes survive a later template load.

In both, state plainly that a key not marked `--secret` is stored in the clear and is copied into `flock.json` and the handover blob with the rest of the sheep's config.

Run `humanizer`, then `rin-voice`, over every paragraph written here before moving on.

- [ ] **Step 4: Build and check the site**

```bash
cd web && npx astro build
```
```bash
cd web && npx astro check
```

Both. `check` is the one that catches a wrong prop: `build` does not typecheck, so a component given a prop it does not have builds clean and renders wrong.

- [ ] **Step 5: Commit**

Two commits, because they are two asks:

```bash
git add web/src/data/cli-reference.generated.txt
git commit
```

Subject: `docs(web): regenerate the CLI reference for the import split`.

```bash
git add web crates/shep-cli/README.md
git commit
```

Subject: `docs(web): document importing a .env`.

---

## Self-review

**Spec coverage.** Decision 1 is Task 1. Decision 2 is Tasks 5 and 6. Decision 3 is Tasks 1 and 6. Decisions 4 and 5 are Task 3. Decision 6 is Task 6, step 5. Decision 7 is Task 3's `SECRETISH` and Task 6's aside. Decision 8 is Tasks 5 and 6. Decisions 9, 10 and 11 are Tasks 2 and 3. Decision 12 is Task 4. Decision 13 is Task 6's module doc. Decision 14 is spread across Tasks 2, 3 and 4, with the correction noted in Task 4. Decision 15 is the file structure above. Decision 16 is Task 7, step 3.

**One correction carried forward.** The spec asks for a manual `Debug` on `Request::SetSheepEnvBatch`. `Debug` is per type; `Request` derives it and `EnvValue` is what makes that safe. Task 4 pins the redaction with an exact-string test instead. The spec is left as written and this paragraph is the record.

**Not covered on purpose.** The spec's "what is not built" list needs no task.
