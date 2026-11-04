# One help standard for verbs and dogs

Status: settled with the maintainer on 2026-11-03, after comparing the help
of gh, docker, kubectl, supabase, uv, cargo and git. Every verb page and
every dog's page follow it, and a dog fits it by declaring data in its
manifest.

## What other CLIs do

| CLI | Top level | A command's page | A plugin's help |
|---|---|---|---|
| gh | Headed groups, one line per command, help topics | Usage, aliases, flags, inherited flags, examples, learn more | The extension's own output |
| docker | Groups, one line per command | Usage, aliases, options | Listed inline from the plugin's metadata, drawn in docker's layout |
| kubectl | Groups by skill level, one line per command | Examples first | The plugin's own output |
| uv, cargo | One line per command | `-h` short, `--help` long | cargo lists external commands by name only |
| supabase | One line per command | Repeats every global flag on every page | None |
| git | Common commands only, `help -a` for the rest | man pages | Listed by name only |

Every one but git describes each command beside its name. Only docker
makes a plugin look native, by drawing it from declared data. shep keeps
two things it already does right: global flags as one footer line, and the
short `-h` beside the long `--help`.

## The top level

`shep --help` lists every visible verb with a description, gh's shape.

```text
A process manager for your flock

Usage: shep <command> [flags]

Getting started
  shep start server.js    start it and keep it alive
  shep flock              see what's running
  shep bleats server      follow its output
  shep startup            bring it back after a reboot

Run things
  start       start an app and keep it alive
  stop        stop one or more sheep
  ...

Dogs and agents
  dogs        list, enable and configure dogs
  install     install a dog from the index
  kelpie      run agents on a ticket board
  log-rotate  rotate logs as they grow

Help topics
  flags       every global flag, in full
  selectors   names, globs, /regex/ and fold:<name>
  exit-codes  what each exit code means

Global flags: --format, --quiet, --style, --host, --home, -h, -V. See shep help flags
Run `shep help <command>` for one command, or `shep welcome` for the tour.
```

- A verb's line is its clap `about`. The listing is rendered from
  `HELP_GROUPS`, which stops being test-only, so the template no longer
  holds a hand-written copy of it.
- An alias sits on its verb's line, `flock, ls`, so the `Aliases` line goes.
- The `Upgrading` line goes. `shep upgrade` and the version skew warning
  already say it where it matters.
- Clap's `Options` block goes. `-h` and `-V` join the global flags line.
- An adopted dog is listed in its group like a verb, with its manifest's
  `verb.description`, or the stub line below.
- About 85 lines, against gh's 72. `shep help all` is not added.

## A verb's page

Every verb page has these sections, in this order. `-h` keeps one line per
argument and flag, `--help` the long text.

| Section | Holds |
|---|---|
| About | One line, at most 60 columns. Up to three more lines under `--help` only |
| Usage | `shep <verb> ...` |
| Arguments, Flags | One line each under `-h` |
| Examples | One to three, each `$ shep ...` |
| Footer | The global flags line, as today |

```text
Stop one or more sheep

Usage: shep stop <selector>...

Arguments
  <selector>...  name, id, glob, /regex/ or fold:<name>. See shep help selectors

Examples
  $ shep stop api
  $ shep stop 'api-*'
  $ shep stop fold:web

Global flags: --format, --quiet, --style, --host, --home. See shep help flags
```

- Examples go in `after_help`, from one helper, so every page spells the
  heading and the `$ ` prefix the same way.
- Prose shared by several verbs moves to a help topic and the verb points to
  it. The selector grammar is the first: its long text leaves `SelectorArgs`
  for `shep help selectors`.

## Help topics

| Topic | Holds | Source |
|---|---|---|
| `flags` | Every global flag, in full | Exists, `cli/global_help.rs` |
| `selectors` | The selector grammar, from today's `SelectorArgs` long text | New |
| `exit-codes` | Each `ExitCode`, its number and when it happens | New, rendered from `exit.rs` |

A topic is matched before clap, as `flags_topic_asked` matches today. The
topic list is one table that the top level and the matcher both read.

## A dog's page

`shep <dog> --help`, `shep <dog> -h` and `shep help <dog>` are always drawn
by shep, never passed through.

```text
Run agents on a ticket board

Usage: shep kelpie <command> [args]

Commands
  board  show the board
  rule   answer a ruling

Examples
  $ shep kelpie board
  $ shep kelpie rule 42 yes looks good

kelpie 0.9.2 from shep-pm/shep-kelpie. Anything after `shep kelpie` goes to the dog.
```

- The last line names the dog's version and its source, from the receipt or
  the manifest, so an operator can tell a dog's page from a verb's.
- `shep <dog> help` and `shep <dog> <sub> --help` still reach the dog. Only
  the bare `--help` forms above are shep's.

### A dog that declares no verb

shep draws a stub, never the dog's raw output:

```text
log-rotate 0.2.5

This dog declares no help. `shep log-rotate help` asks the dog itself.
```

The description comes from the cached index row when there is one. In
`shep --help` the dog's line is the index description, or its name alone.

## The manifest

Additive, read with `serde(default)`. A shep that predates them ignores
them, so `MANIFEST_VERSION` does not move for this.

| Field | Today | After |
|---|---|---|
| `verb.description` | One line | At most 60 columns. Adopt warns past that, one line, and still adopts |
| `verb.subcommands[].name`, `.about` | Present | Unchanged |
| `verb.subcommands[].usage` | None | Optional, the words after the name, as `<id> yes\|no <note>` |
| `verb.examples` | None | Zero to three command lines, each without `$ ` |

- A Rust dog builds `verb` from its own clap tree: `Verb::from_clap(&Command)`
  in `shep_client::dogs`, behind a new `clap` feature (`# Option:` comment,
  off by default). It reads the about, each visible subcommand's name, about
  and usage, and examples from a `Command` extension the helper documents.
- A dog in any language writes the JSON.
- `shep-kelpie` and `shep-log-rotate` each declare a `verb`, in their own
  repos, once a shep-client carrying `examples` is published.

## Checks

- A test holds every visible verb's about to 60 columns and at least one
  example.
- A test holds every help string, verb and topic, to no em dash.
- The existing `help.rs` tests keep pinning each verb to one group and to
  the docs generator's `VERBS` list.

## Not in this

- `metrics` and `bark` keep living under `shep dogs`. No `shep metrics` verb.
- No `shep help all`.

## Docs

The CLI reference is regenerated from the binary. `writing-a-dog.astro`
gains a section on a dog's help: the manifest fields, `Verb::from_clap`, and
the stub. `getting-started.astro` is checked against the new top level.

## Order of work

1. Help topics: `selectors` and `exit-codes`, the one topic table.
2. The top level rendered from `HELP_GROUPS` with abouts, aliases inline.
3. Examples on every verb page, with the helper and the tests.
4. The manifest fields and `Verb::from_clap`.
5. The dog page and the stub, drawn by shep.
6. `shep-kelpie` and `shep-log-rotate` declare their verbs, after the
   shep-client release that carries step 4.
