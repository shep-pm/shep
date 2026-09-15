//! The clap command tree: [`Cli`], [`Commands`], and every argument struct.
//!
//! This is the whole parse surface of the `shep` binary in one place, pure
//! tier (spec §11): it compiles and its tests run on every target, Windows
//! included, so `Cli::command().debug_assert()` and the alias tests below
//! cover a platform that cannot build the rest of this crate.
//!
//! This module owns every argument struct in the tree, even the ones whose
//! command is not wired up yet — the whole parse surface lives in one
//! portable file rather than accreting piecemeal as each verb lands.

mod help;
mod verbs;

use core::fmt;
use std::net::IpAddr;
use std::path::PathBuf;

use shep_core::config::ResetDepth;

use help::{HELP_TEMPLATE, version_text};

pub use verbs::Commands;

/// The `shep` command line.
// `bin_name = "shep"` below is load-bearing, not decoration. Without it, clap
// renders every `Usage:` line from `argv[0]` rather than from `name` — so
// `shep-runtime --help` prints `Usage: shep-runtime runtime ...` and
// `shep-dev --help` prints `Usage: shep-dev dev ...` when both alias
// binaries are built and run with no override.
// Pinned so every rendering of a verb's own usage line reads `shep <verb>`
// regardless of which of the three `[[bin]]` targets produced it — the alias
// binaries are convenience entrypoints for exactly that invocation, not
// commands in their own right, and their own `--help` should say so.
//
// A `//` comment, not `///`, deliberately: clap renders a doc comment as
// `long_about`, so as a doc comment this paragraph WAS the opening of
// `shep --help` for three phases. See the test
// `the_top_level_help_carries_no_implementation_notes`.
#[derive(Debug, clap::Parser)]
#[command(
    name = "shep",
    bin_name = "shep",
    version,
    long_version = version_text(),
    about = "A process manager for your flock",
    propagate_version = true,
    help_template = HELP_TEMPLATE,
    after_help = "Run `shep help <command>` for one command, or `shep welcome` for the tour."
)]
pub struct Cli {
    /// Flags valid on every subcommand.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// The verb being invoked.
    #[command(subcommand)]
    pub command: Commands,
}

/// Flags valid on every subcommand, folded into [`Cli`] via `#[command(flatten)]`.
#[derive(Debug, clap::Args)]
pub struct GlobalArgs {
    /// Output format
    #[arg(long, global = true, value_enum, default_value_t = Format::Table)]
    pub format: Format,
    /// Suppress non-essential output
    ///
    /// Currently narrows `bleats`' own notices (a dropped-events count, a
    /// daemon-shutdown notice, ...): diagnostics distinct from a sheep's
    /// own line or a real error, both of which still print regardless.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// How much this invocation dresses up its output: `full`, `plain`, or
    /// `bare`
    ///
    /// Wins over `$SHEP_STYLE` and `shep.toml`'s `[style] level`. Omit to
    /// let those decide; `shep style` reports which one answered.
    // The precedence order above is `style::resolve`'s; this field is only
    // the flag's own place in it.
    #[arg(long, global = true, value_enum)]
    pub style: Option<crate::style::StyleLevel>,
    /// Talk to a different shepherd
    ///
    /// Mostly plumbing: `shep dev` sessions, a system-wide flock, tests. You
    /// almost certainly want the default, ~/.shep. Must be an absolute path:
    /// a relative one would name a different flock from every directory.
    // Declared last on purpose. It was the first global option anyone read,
    // which announced it as a choice when it is really the daemon's
    // data-root. `{options}` renders in declaration order and ignores
    // `help_heading`, so position is the only lever available here: a `Less
    // common` section would need `{all-args}`, which re-emits the
    // alphabetical command wall this template exists to replace.
    //
    // `//`, not `///`, for the same reason the note above `Cli` is one. This
    // paragraph shipped in `shep --help` for exactly one build before the
    // render was read.
    #[arg(long, global = true, env = "SHEP_HOME")]
    pub home: Option<PathBuf>,
}

/// `--format`'s two shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Human-readable columns (the default).
    Table,
    /// A versioned JSON envelope, one object per invocation.
    Json,
}

/// Which init system a unit is written for.
///
/// Five variants, all constructible on every target: `--init` lets an
/// operator name one directly, which is also what lets a macOS machine
/// exercise the systemd, openrc and rc.d renderers at all. Selection without
/// the flag is `commands::startup::current_init` — a runtime probe on Linux,
/// where systemd and openrc share one target triple, and a compile-time fact
/// everywhere else, where nothing else the target could be exists.
///
/// It lives in `cli.rs` rather than beside the renderers because `cli.rs`
/// compiles on **every** target while `mod commands` is `#[cfg(unix)]`. A
/// field on `StartupArgs` naming a type from a unix-only module breaks
/// `cargo check --workspace --all-targets --all-features --target
/// x86_64-pc-windows-gnu`, which is a phase-gate command. `Format` above is
/// the precedent: a `clap::ValueEnum` the parse surface owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum Init {
    /// Linux + systemd: a unit file, `Type=notify`.
    Systemd,
    /// Linux + openrc: an `openrc-run` script. No readiness protocol — see
    /// the renderer's own doc.
    Openrc,
    /// macOS: a `LaunchDaemon` plist.
    Launchd,
    /// FreeBSD: an `/etc/rc.subr` script under `/usr/local/etc/rc.d`.
    FreebsdRc,
    /// OpenBSD: an `/etc/rc.d/rc.subr` script under `/etc/rc.d`.
    OpenbsdRc,
}

/// Arguments to `shep start` and `shep add`.
///
/// One struct for both, the precedent [`SelectorArgs`] already sets for
/// `stop`/`restart`/`reload`/`delete`: the two verbs take the same targets,
/// resolve them the same way, and differ only in whether anything is spawned
/// at the end. A second struct would be the same eight fields with the same
/// meanings, and a flag added to one of them and not the other.
#[derive(Debug, clap::Args)]
pub struct StartArgs {
    /// Selectors, script paths, Flockfiles, or `-` to read Flockfile JSON
    /// from stdin
    ///
    /// A target is resolved in four tiers and the first one that matches
    /// wins: a sheep the flock already has, by id or by name; then a fold,
    /// written either as `fold:<name>` or as the bare fold name; then a
    /// Flockfile, by its extension; then a path on disk, started as a
    /// script.
    ///
    /// So `shep start backed` starts the fold `backed` even when a file
    /// called `backed` is sitting in the current directory. Write `./backed`
    /// to mean the file: a sheep name may never contain a path separator, so
    /// a target carrying one is always a path.
    ///
    /// The wildcard selectors work here too, and mean what they mean
    /// everywhere else: `all`, `/regex/`, and glob patterns such as
    /// `web-*`. They reach only sheep the flock already has, since there is
    /// nothing to register. A sheep already running is reported and left
    /// alone; `restart` is the verb that replaces one, and `add` never
    /// starts anything in the first place.
    ///
    /// Omit the targets to read the Flockfile in the current directory. With
    /// no Flockfile there, `shep start` brings a shepherd up with nothing
    /// running yet, and `shep add` has nothing it could register.
    ///
    /// A target may be preceded by `NAME=VALUE` assignments, read the way a
    /// shell reads them: `shep start KOJI_TOKEN=secret ./koji` registers koji
    /// with that variable set and records it, so a later `shep start koji`
    /// needs no assignment. A name takes a letter or `_` and then letters,
    /// digits or `_`, which is what makes `./A=1` a path and `1A=1` a target.
    /// Quote the value and never the pair: `shep start "A=x y" ./koji`.
    ///
    /// Assignments take one target, and it must be a script path or a sheep
    /// the flock already has. A Flockfile or a fold can name several sheep,
    /// and an assignment names none of them. The value passes through this
    /// command line, so it reaches `ps` and your shell history; write
    /// `{{secret:NAME}}` to read a credential from `shep secret` instead.
    ///
    /// Several are handled in turn, not atomically: if the second fails the
    /// first has already landed, and the exit code is the first failure.
    /// `--name` is refused with more than one, since a name is unique to one
    /// sheep.
    #[arg(num_args = 0..)]
    pub targets: Vec<String>,
    /// Name for this sheep (script form only)
    #[arg(long)]
    pub name: Option<String>,
    /// Fold to place this sheep in
    #[arg(long)]
    pub fold: Option<String>,
    /// Working directory to run in (default: where you ran `shep start`)
    #[arg(long)]
    pub cwd: Option<String>,
    /// Interpreter to run the script with, overriding both shep.toml's
    /// extension mapping and a Flockfile app's own interpreter field.
    ///
    /// The precedence, lowest to highest: shep.toml's interpreters table
    /// (matched against the script's extension), then a Flockfile's own
    /// interpreter for that app, then this flag. shep never guesses an
    /// interpreter on its own; every one of those three is something an
    /// operator wrote down. Pass "none" to run the script directly,
    /// overriding a mapping or a Flockfile that would otherwise pick one.
    #[arg(long)]
    pub interpreter: Option<String>,
    /// Read TARGET as a Flockfile rather than as a script path.
    ///
    /// Required for a `.js` Flockfile and the only way to reach one: shep
    /// reads a `.js` config by running it through node, which is arbitrary
    /// code execution, so it never happens because a file merely has that
    /// extension. Without this flag `shep start server.js` starts
    /// `server.js` as a script, which is what it has always meant.
    #[arg(long)]
    pub flockfile: bool,
    /// Widen a Flockfile load past its additive default: append nothing,
    /// overwrite instead. A mode touches only what its name says; see the
    /// four below. Refused when the target supplies no template to reset
    /// to: a sheep name reads no file, and a bare script path is a command
    /// line rather than a file.
    ///
    /// The value is required, with an equals sign: `--reset=file`, never
    /// `--reset file`. `targets` is a greedy variadic positional, and a
    /// space-separated value next to one of those is where argument parsing
    /// gets ambiguous.
    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = ""
    )]
    pub reset: Option<ResetMode>,
}

/// How far a `shep start`/`shep add` load widens past its additive default.
///
/// A CLI-local mirror of [`ResetDepth`], not `ResetDepth` itself: shep-core
/// carries no `clap` dependency, and giving a wire protocol type a
/// command-line-parser dependency to save this mapping would be the wrong
/// trade. [`ResetDepth::None`] has no flag value of its own -- omitting
/// `--reset` entirely is how an operator asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum ResetMode {
    /// Put back what the template declares, `env` kept.
    File,
    /// Put back every setting but `env`, declared or not.
    Policy,
    /// Put back only `env`.
    Env,
    /// Put back everything, `env` included, and drop the override record.
    All,
}

impl ResetMode {
    /// Maps to the wire type `Request::ApplyConfig` actually carries.
    #[must_use]
    pub fn to_depth(self) -> ResetDepth {
        match self {
            Self::File => ResetDepth::File,
            Self::Policy => ResetDepth::Policy,
            Self::Env => ResetDepth::Env,
            Self::All => ResetDepth::All,
        }
    }
}

/// Arguments to `shep serve`.
///
/// `PartialEq` is derived for one reason and it is a test: Step 7.4's
/// round-trip asserts the whole struct, so a field added without teaching
/// `sheep_args` about it fails by construction rather than by somebody
/// remembering to extend a list of `assert!`s.
#[derive(Debug, PartialEq, Eq, clap::Args)]
pub struct ServeArgs {
    /// Directory to serve
    pub root: PathBuf,
    /// Port to listen on
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    /// Address to bind. Loopback unless you say otherwise — a wider bind
    /// publishes every file under the directory to anything that can reach
    /// the port.
    #[arg(long, default_value = "127.0.0.1")]
    pub bind: IpAddr,
    /// Name for this sheep (default: the directory's own name)
    #[arg(long)]
    pub name: Option<String>,
    /// Fold to place this sheep in
    #[arg(long)]
    pub fold: Option<String>,
    /// Serve index.html for paths that do not exist, for a single-page app.
    /// Only for requests that accept HTML, so a missing script still 404s.
    #[arg(long)]
    pub spa: bool,
    /// List a directory that has no index.html. Off by default: a listing
    /// publishes every filename under it.
    #[arg(long)]
    pub listing: bool,
    /// Serve files and directories whose names begin with a dot. Off by
    /// default: serving a project directory would otherwise publish `.env`
    /// and the whole `.git` history. The one real use is
    /// `.well-known/acme-challenge`.
    #[arg(long)]
    pub hidden: bool,
    /// Follow symlinks under the docroot, reopening the check-then-open race
    /// refused by default. Needed for deploy layouts like
    /// `current -> releases/2026-08-15`; off unless you ask for it.
    #[arg(long)]
    pub follow_symlinks: bool,
    /// File holding one `user:password` line, mode 0600, required on every
    /// request. Sent over plain HTTP — base64, not encryption.
    #[arg(long)]
    pub auth: Option<PathBuf>,
    /// Serve in this terminal instead of registering a sheep.
    ///
    /// This is also how the registered sheep runs: the command line in
    /// `shep describe` is this one with the flag on the end.
    #[arg(long)]
    pub foreground: bool,
}

/// Arguments shared by every verb that targets an existing selection of the
/// flock (`stop`, `restart`, `reload`, `delete`, `describe`, `thatlldo`).
///
/// The selector is required on every one of them, because every one of them
/// acts on something. `flush` has the same rule and its own struct
/// ([`FlushArgs`]) only because it has a second target that is not a
/// selection at all.
///
/// Required means no `default_value` on the field below, and that one
/// attribute is the whole of it — adding one would turn a bare `shep stop`
/// into `shep stop all` for every verb in the list at once. It is pinned by
/// this module's own `a_selector_taking_verb_refuses_to_run_without_one`
/// (named rather than linked: that module is `#[cfg(test)]`, so an intra-doc
/// link to it does not resolve under `cargo doc`).
#[derive(Debug, clap::Args)]
pub struct SelectorArgs {
    /// One or more: name, id, `name:slot`, `all`, `zeus-*`, `/regex/`, `fold:<name>`
    ///
    /// Several are applied in turn, not atomically: `shep stop a b c` where
    /// `b` matches nothing still stops `a` and `c`, and the exit code is the
    /// first failure.
    ///
    /// A pattern carrying `*`, `?`, `[` or `{` is a glob, anchored, so
    /// `zeus-*` selects `zeus-auth` and not `my-zeus-auth`. Quote it: your
    /// shell expands `zeus-*` against filenames first, and zsh refuses
    /// outright when none match. A name with no such character is exact, so
    /// `web.1` is the sheep called `web.1`.
    #[arg(required = true, num_args = 1..)]
    pub selectors: Vec<String>,
}

/// The floor and the default for `shep flock --follow`'s `--interval`, in
/// seconds.
///
/// One second, from two measurements rather than taste. `sysinfo` reports no
/// CPU at all over a window shorter than its own
/// `MINIMUM_CPU_UPDATE_INTERVAL`, which is 200 ms, so a faster cadence buys
/// a column of dashes. And one redraw costs the shepherd a
/// `Request::ListFlock` plus a host sample measured at 5.6 ms a tick on
/// macOS (`crate::host`'s own module doc): 0.6% of a core at this floor, and
/// 5.6% of one at the 100 ms somebody would otherwise reach for.
///
/// Seconds rather than milliseconds because the operator types it, and
/// nothing between 200 ms and 1 s is worth the unit confusion.
pub(crate) const FOLLOW_INTERVAL_FLOOR_SECONDS: u64 = 1;

/// Arguments to `shep flock`.
///
/// A listing is a moment, and this moment drifts: a sheep that started
/// after the list printed, one restarting. `--follow` keeps the moment
/// current.
#[derive(Debug, clap::Args)]
pub struct FlockArgs {
    /// Keep the listing current: redraw it in place until Ctrl+C
    ///
    /// One listing to start, then a fresh one every `--interval` seconds
    /// while the shepherd stays reachable, each one painted over the last.
    /// A summary of the machine rides above the tables. Ctrl+C leaves an
    /// exit of zero, a stopped shepherd leaves the code its refusal
    /// carries: an interrupted follow and a dead shepherd must not read as
    /// the same thing.
    ///
    /// Needs a live shepherd: a saved roll is a single moment, and a
    /// follow's whole reason is the moments after it. Needs a terminal
    /// too, and refuses `--format json`, since neither has anywhere to put
    /// a redraw.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub follow: bool,
    /// Seconds between redraws, with `--follow`
    ///
    /// The floor is one second, which is also the default. Below it the
    /// listing costs the shepherd more than it tells the operator, and
    /// `sysinfo` reports no CPU at all over a window that short.
    #[arg(
        long,
        default_value_t = FOLLOW_INTERVAL_FLOOR_SECONDS,
        value_name = "SECONDS",
        value_parser = clap::value_parser!(u64).range(FOLLOW_INTERVAL_FLOOR_SECONDS..),
        requires = "follow"
    )]
    pub interval: u64,
}

/// Arguments to `shep stock`.
///
/// Not [`SelectorArgs`], and this is the only lifecycle verb that is not.
/// `instances` is a per-app number, so the target is an app NAME: no `all`,
/// no `/regex/`, no `fold:` — a selector matching two apps would have to mean
/// either four each or four in total, and neither reading is more obviously
/// right.
#[derive(Debug, clap::Args)]
pub struct StockArgs {
    /// The app's name
    pub name: String,
    /// How many instances it runs afterwards
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub count: u32,
}

/// Arguments to `shep trigger`.
///
/// Not [`SelectorArgs`]: this verb needs two more positionals than a
/// selector, `action` and the optional `params` after it, so it carries its
/// own struct rather than widening the one every other selector-taking verb
/// shares. The selector is still required — no `default_value`, matching
/// `stop`/`restart`/`reload`/`delete`/`describe` — for the same reason: this
/// reaches a running app, so the operator names the target rather than
/// trigger one against the whole flock by accident.
#[derive(Debug, clap::Args)]
pub struct TriggerArgs {
    /// name, id, `name:slot`, `all`, `/regex/`, or `fold:<name>`
    pub selector: String,
    /// Action name — free-form, defined by the app
    pub action: String,
    /// Argument text for the action, passed through to the app verbatim
    pub params: Option<String>,
}

/// Arguments to `shep signal`.
///
/// Not [`SelectorArgs`]: this verb needs a second positional. The selector
/// stays required — no `default_value` — for the reason every
/// running-process verb's does: an accidental `shep signal` should be a usage
/// error, never a flock-wide SIGHUP.
#[derive(Debug, clap::Args)]
pub struct SignalArgs {
    /// name, id, `name:slot`, `all`, `/regex/`, or `fold:<name>`
    pub selector: String,
    /// Signal name, e.g. `SIGHUP` or `hup`
    pub signal: String,
}

/// Arguments to `shep whisper`.
///
/// Not [`SelectorArgs`]: this verb needs a second positional, the line
/// itself. The selector stays required — no `default_value` — for the same
/// reason every running-process verb's does: an accidental `shep whisper`
/// should be a usage error, never sent to the whole flock.
#[derive(Debug, clap::Args)]
pub struct WhisperArgs {
    /// name, id, `name:slot`, `all`, `/regex/`, or `fold:<name>`
    pub selector: String,
    /// The line, without a trailing newline — shep adds exactly one
    pub line: String,
}

/// Arguments to `shep flush`.
///
/// # Why a flag and not a reserved selector name
///
/// The shepherd's own `shepd.out.log`/`shepd.err.log` are the second thing
/// this verb can empty, and they are NOT a sheep — nothing about them is
/// expressible as a selector. Spelling them `shep flush shep` would make one
/// name mean something different depending on the Flockfile, since nothing
/// stops an app being called `shep`, and an operator who named one that would
/// find `shep flush shep` quietly emptying the wrong files. A flag cannot
/// collide with anything.
///
/// # Why it replaces the selector rather than composing with it
///
/// `--daemon` conflicts with the selector, so `shep flush all --daemon` is a
/// usage error rather than "both". Three reasons, in order of weight: the two
/// halves answer with different shapes — sheep against files — and one
/// invocation renders one payload into one envelope; the daemon's own logs
/// are the one target the maintainer asked never to be reached without being named, and
/// a flag that rode along with `all` would be reached by every operator who
/// ever typed `shep flush all --daemon` out of habit; and the shepherd's logs
/// are not a sheep's, so folding them into a flock answer would mean
/// inventing a row for something with no id and no name.
///
/// The selector stays required in every other case — `required_unless_present`
/// rather than a `default_value`, so a bare `shep flush` is still the usage
/// error it has always been, never "empty every log in the flock".
#[derive(Debug, clap::Args)]
pub struct FlushArgs {
    /// name, id, `name:slot`, `all`, `/regex/`, `fold:<name>` (required unless --daemon)
    #[arg(required_unless_present = "daemon", conflicts_with = "daemon")]
    pub selector: Option<String>,
    /// Empty the shepherd's own logs instead of any sheep's
    #[arg(long)]
    pub daemon: bool,
}

/// Arguments to `shep barks`.
///
/// No selector, and no `--daemon`-shaped flag either — `barks.jsonl` is one
/// file for the whole `$SHEP_HOME`, holding both the bark dog's own alerts
/// and the ones the shepherd wrote itself when an enabled dog exhausted its
/// restart budget, so there is no population within it to select a subset
/// of the way `flush` selects sheep.
#[derive(Debug, clap::Args)]
pub struct BarksArgs {
    /// Show only the last N barks
    #[arg(long)]
    pub tail: Option<usize>,
}

/// Arguments to `shep set`.
#[derive(Debug, clap::Args)]
pub struct KvSetArgs {
    /// The key
    pub key: String,
    /// The value
    pub value: String,
}

/// Arguments to `shep get`.
#[derive(Debug, clap::Args)]
pub struct KvGetArgs {
    /// The key; omit to list every key
    pub key: Option<String>,
}

/// Arguments to `shep unset`.
///
/// `--all` rather than a reserved key name, for the reason [`FlushArgs`]'s own
/// doc gives about `shep flush shep`: nothing stops an operator having a key
/// called `all`, and `shep unset all` would then mean something different
/// depending on their own store. A flag cannot collide.
#[derive(Debug, clap::Args)]
pub struct KvUnsetArgs {
    /// The key to remove
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub key: Option<String>,
    /// Remove every key
    #[arg(long)]
    pub all: bool,
}

/// Arguments to `shep secret`.
///
/// `Debug` is written out rather than derived (IR-41) so a field added here
/// cannot print in the clear by default; the value itself is redacted by
/// [`SecretCommand`]'s own impl.
#[derive(clap::Args)]
pub struct SecretArgs {
    /// Which of the four things to do to the store.
    #[command(subcommand)]
    pub command: SecretCommand,
}

impl fmt::Debug for SecretArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretArgs")
            .field("command", &self.command)
            .finish()
    }
}

/// `shep secret`'s subcommands.
///
/// `Set` carries the operator's own plaintext value, so `Debug` prints its
/// length and never the value (IR-41). Exact-string-tested below
/// (`secret_command_debug_does_not_leak`), so restoring the derive fails
/// that test instead of silently reopening the leak.
#[derive(clap::Subcommand)]
pub enum SecretCommand {
    /// Store a value
    ///
    /// The value is a positional argument by default, so it is visible in
    /// `ps` and in this shell's history for as long as the command runs.
    /// Pass `--stdin` to keep it out of both.
    Set {
        /// The key
        key: String,
        /// The value; required unless --stdin is given
        #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
        value: Option<String>,
        /// Which environment; omit for every environment
        #[arg(long)]
        env: Option<String>,
        /// Read the value from stdin; the positional form is visible in
        /// `ps` and in shell history
        #[arg(long)]
        stdin: bool,
    },
    /// Print a value back, if `[secrets] allow_read` is on
    Get {
        /// The key
        key: String,
        /// Which environment; omit to use `[daemon] environment`'s
        /// default, which an app's own environment may override
        #[arg(long)]
        env: Option<String>,
    },
    /// Remove a value
    Unset {
        /// The key
        key: String,
        /// Which environment; omit for the every-environment slot
        #[arg(long)]
        env: Option<String>,
    },
    /// List keys and the environments each has a value for
    List,
}

impl fmt::Debug for SecretCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set {
                key,
                value,
                env,
                stdin,
            } => {
                let value = value.as_ref().map_or_else(
                    || "None".to_string(),
                    |value| format!("Some(<{} bytes>)", value.len()),
                );
                f.debug_struct("Set")
                    .field("key", key)
                    .field("value", &format_args!("{value}"))
                    .field("env", env)
                    .field("stdin", stdin)
                    .finish()
            }
            Self::Get { key, env } => f
                .debug_struct("Get")
                .field("key", key)
                .field("env", env)
                .finish(),
            Self::Unset { key, env } => f
                .debug_struct("Unset")
                .field("key", key)
                .field("env", env)
                .finish(),
            Self::List => f.write_str("List"),
        }
    }
}

/// Arguments to `shep fold`.
#[derive(Debug, clap::Args)]
pub struct FoldArgs {
    /// The fold to list
    pub name: String,
}

/// `shep dogs`, and the index of dogs you could adopt.
#[derive(Debug, clap::Args)]
pub struct DogsArgs {
    /// List the dogs published in the community index instead of the ones
    /// this shepherd is running. Needs no shepherd.
    #[arg(long)]
    pub available: bool,
    /// Narrow the listing to entries whose name, package or description
    /// contains this text, case-insensitively.
    #[arg(value_name = "FILTER")]
    pub filter: Option<String>,
}

/// Arguments to `shep disable`/`shep rehome`, and to the hidden `shep dog`
/// re-exec target.
///
/// One struct for all three, matching [`StartupArgs`]'s own precedent: a
/// dog is named, never selected — `SelectorArgs`' grammar (`all`, `/regex/`,
/// `fold:<name>`) answers "which of the flock", and a dog is not the flock.
/// `shep enable` shares this shape too, but carries a second, hidden field
/// ([`EnableArgs`]) that none of the three below has any use for, so it
/// gets a struct of its own rather than widening this one for verbs that
/// would never touch the extra field.
#[derive(Debug, clap::Args)]
pub struct DogArgs {
    /// The dog's name, the `[<name>]` config key in `dogs.toml`
    pub name: String,
}

/// Arguments to `shep enable`.
///
/// [`DogArgs`] plus one hidden field: `--exec` is pm2's own spelling of
/// `shep adopt`, kept as a working alias so muscle memory carries over —
/// `#[arg(hide = true)]`, not `#[command(alias = ..)]`, because the alias
/// is on an argument, not the subcommand itself. `shep enable --exec <path>
/// <name>` parses here and is routed to [`super::commands::dogs::adopt`] by
/// `main`'s own dispatch, never handled by `enable` itself: a dog already
/// built into this binary has no path to vet, so `enable` cannot carry out
/// what `adopt` does.
///
/// `name` here is a required positional, unlike [`AdoptArgs`]'s own
/// (optional, defaulted from the binary's file stem) — pm2's own spelling
/// carries no such default, and `enable --exec` exists only to keep that
/// spelling working verbatim, not to gain `adopt`'s newer conveniences. A
/// reader relying on the two shapes agreeing is a mistake nothing but a
/// test catches — see `main.rs`'s
/// `the_hidden_pm2_spelling_reaches_adopt_with_the_arguments_the_right_way_round`.
#[derive(Debug, clap::Args)]
pub struct EnableArgs {
    /// The dog's name, the `[<name>]` config key in `dogs.toml`
    pub name: String,
    /// Hidden pm2-spelling alias for `shep adopt`: routes to `adopt` with
    /// this flag's value as the binary path
    #[arg(long, hide = true)]
    pub exec: Option<PathBuf>,
}

/// Arguments to `shep adopt`.
///
/// `path` is the one positional; `--name` is optional and defaults to the
/// binary's file stem with a leading `shep-` stripped, the way `cargo`
/// strips `cargo-` from its own external subcommands (`shep-log-rotate`
/// defaults to `log-rotate`). Previously both were required positionals,
/// name first (`adopt <name> <path>`) — a breaking CLI change, decision
/// The maintainer: `shep adopt <path>` alone now works for a binary whose name is
/// already the name you want, matching `shep start <script>`'s own
/// optional `--name`.
#[derive(Debug, clap::Args)]
pub struct AdoptArgs {
    /// Path to the dog's binary, vetted before `shep.toml` is touched.
    /// Resolved before vetting: as given, with a leading `~/` expanded, or
    /// looked up on `$PATH` if it names no directory — first hit wins.
    pub path: PathBuf,
    /// The dog's name, the `[<name>]` config key in `dogs.toml`. Defaults
    /// to the binary's file stem with a leading `shep-` stripped.
    #[arg(long)]
    pub name: Option<String>,
}

/// The selector the verbs that take an optional one fall back to.
///
/// One owner for the string, shared rather than spelled twice: `bleats` and
/// `reopen` default to the same thing on purpose, and a copy that drifted
/// would leave one of them quietly targeting something else.
const DEFAULT_SELECTOR: &str = "all";

/// How much history `shep bleats` prints before it starts following.
///
/// Fifteen because that is what pm2 has trained every operator to expect,
/// and because the number only has to be large enough to carry the reason a
/// sheep died. It is small enough that following a healthy flock still
/// starts nearly empty.
pub const DEFAULT_BLEAT_LINES: usize = 15;

/// Arguments to `shep bleats` (alias `logs`).
#[derive(Debug, clap::Args)]
pub struct BleatsArgs {
    /// Which sheep (default: all)
    #[arg(default_value = DEFAULT_SELECTOR)]
    pub selector: String,
    /// Print the tail of each sheep's log file and exit, instead of following
    #[arg(long)]
    pub no_follow: bool,
    /// How many existing lines of each stream to print before following
    ///
    /// A sheep that already crashed has said everything it is going to say,
    /// so following alone shows an empty screen while the reason sits in the
    /// file. This prints that much history first, then follows.
    ///
    /// Counted per stream, so the default prints up to this many lines of
    /// stdout and up to this many of stderr for each matched sheep. Narrow
    /// it with `--out` or `--err`.
    ///
    /// `0` prints no history at all, following only what arrives next.
    #[arg(long, default_value_t = DEFAULT_BLEAT_LINES, value_name = "N")]
    pub lines: usize,
    /// Only stderr
    #[arg(long, conflicts_with = "out")]
    pub err: bool,
    /// Only stdout
    #[arg(long, conflicts_with = "err")]
    pub out: bool,
}

/// Arguments to `shep lookout`.
#[derive(Debug, clap::Args)]
pub struct LookoutArgs {
    /// Close the dashboard's action gate. Actions are permitted by default.
    ///
    /// With the gate open, `x` (stop), `R` (restart) and `L` (reload) each
    /// arm a confirm instead of acting on the keypress that pressed it;
    /// Enter sends the request, any other key cancels, and an unanswered
    /// confirm expires after ten seconds. Closed, all three refuse outright.
    ///
    /// A guard against a keystroke in a window you were reading, not a
    /// security boundary: lookout runs as you, so anything it could do you can
    /// already do with `shep stop`. Can also be closed with `shep set
    /// lookout.allow_control false`.
    #[arg(long)]
    pub read_only: bool,
}

/// Arguments to `shep reopen`.
///
/// The selector is optional, defaulting to [`DEFAULT_SELECTOR`], where
/// `stop`/`restart`/`delete` all demand one: those destroy something, and
/// a reopen destroys nothing — it swaps a file handle for another handle on
/// the same path. Rotating every sheep at once is also the ordinary case, a
/// `postrotate` stanza having just renamed the whole log directory.
#[derive(Debug, clap::Args)]
pub struct ReopenArgs {
    /// Which sheep (default: all)
    #[arg(default_value = DEFAULT_SELECTOR)]
    pub selector: String,
}

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
    ///
    /// Reads `--from`, or `~/.pm2/dump.pm2` if it names nothing — whichever
    /// `pm2 save` last wrote. Every clustered app is named on stderr: shep
    /// binds nothing, so N instances on one port need the app to set
    /// `SO_REUSEPORT` itself, or the second instance hits EADDRINUSE at
    /// start. Every env key the dump carried that was neither declared nor
    /// recognizable session junk is named on stderr too, and left out of
    /// the Flockfile, for the operator to decide.
    Pm2(ImportPm2Args),
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
    /// grammar does not accept refuses the whole import. A refusal reached
    /// before the secret store is written leaves both stores alone; one
    /// reached after counts the keys it left there, which nothing
    /// references until the import is re-run.
    Env(ImportEnvArgs),
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

/// Arguments to `shep import env`.
///
/// `Debug` is derived: every field is a path, a name or a pattern. The
/// values live in the file this names, never in the arguments, which is
/// also why there is no `--stdin`.
#[derive(Debug, clap::Args)]
pub struct ImportEnvArgs {
    /// The `.env` to read
    ///
    /// A `{{...}}` in a plain value reaches the app as the literal text the
    /// file wrote, not as a shep template reference.
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
    /// `environment` field, or `[daemon] environment` when it has none, and
    /// so is never the `all` slot that every environment reads. Naming
    /// `all` here writes that slot, as `shep secret set --env all` does.
    #[arg(long)]
    pub env: Option<String>,
    /// Print what would be written, and write nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite keys that already hold a different value
    #[arg(long)]
    pub force: bool,
}

/// Arguments shared by `shep startup` and `shep unstartup`.
///
/// One struct for both verbs: the unit is named after the user it runs the
/// shepherd as, and, since Task 6, after which init system it targets.
/// `--home` is read from [`GlobalArgs`] by `startup` and ignored by
/// `unstartup`, which removes a unit rather than writing one.
#[derive(Debug, clap::Args)]
pub struct StartupArgs {
    /// The user the unit runs the shepherd as (default: $SUDO_USER, else the invoking user)
    #[arg(long)]
    pub user: Option<String>,
    /// Write a unit for this init system instead of the detected one.
    ///
    /// `unstartup` takes it too: a unit installed under one init has to be
    /// removable after the host has changed to another.
    #[arg(long, value_enum)]
    pub init: Option<Init>,
}

/// Arguments to `shep completions`.
#[derive(Debug, clap::Args)]
pub struct CompletionArgs {
    /// Shell to generate a completion script for
    #[arg(value_enum)]
    pub shell: clap_complete::aot::Shell,
}

/// Arguments to `shep style`.
#[derive(Debug, clap::Args)]
pub struct StyleArgs {
    /// `full`, `plain`, or `bare`
    ///
    /// Sets `shep.toml`'s `[style] level`. Omit to report the level
    /// currently in force instead of changing it.
    // The same `StyleLevel` grammar `--style` parses, so `shep style loud`
    // and `shep --style loud` are rejected identically -- see
    // `style_verb_parses_the_same_grammar_as_the_style_flag` below.
    #[arg(value_enum)]
    pub level: Option<crate::style::StyleLevel>,
}

/// Arguments to the hidden `shep daemon` subcommand.
///
/// The last four are the CLI-flag layer of spec §5's `file < env < flags`,
/// one per `SHEP_*` variable `DaemonConfig::load` already reads. They live
/// here rather than on `GlobalArgs` because they configure **the shepherd**,
/// and this is the only invocation that runs one — `--log-level` on
/// `shep flock` would configure nothing.
///
/// Their real audience is an init unit's `ExecStart`, which can now say
/// `shep daemon --foreground --log-level info` without a config file.
#[derive(Debug, clap::Args)]
pub struct DaemonArgs {
    /// What to do to the shepherd. Omit to BE the shepherd.
    ///
    /// `None` is the boot path and the reason this is optional at all: the
    /// binary daemonizes by re-execing itself with `daemon` and nothing
    /// else (`crate::launch::launch_daemon`), so a required subcommand here
    /// would break daemonization itself rather than merely a verb.
    #[command(subcommand)]
    pub cmd: Option<DaemonCmd>,
    /// Boot without restoring the saved muster roll
    #[arg(long)]
    pub no_restore: bool,
    /// Run supervised by an init system: do not expect to have been
    /// daemonized, and report readiness once the flock is back
    #[arg(long)]
    pub foreground: bool,
    /// Emit the shepherd's own logs as JSON lines (overrides shep.toml and
    /// SHEP_LOG_JSON). Accepts 1, 0, true, false; bare means true.
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        default_missing_value = "true",
        value_parser = bool_flag
    )]
    pub log_json: Option<bool>,
    /// Lowest severity of the shepherd's own records that reaches its log
    #[arg(long, value_name = "LEVEL", value_parser = log_level_flag)]
    pub log_level: Option<shep_core::config::LogLevel>,
    /// Control-socket path override
    #[arg(long, value_name = "PATH")]
    pub socket: Option<PathBuf>,
    /// Longest a cron worker sleeps before re-deriving its next occurrence
    #[arg(long, value_name = "DURATION", value_parser = duration_flag)]
    pub max_cron_sleep: Option<shep_core::values::UpDuration>,
}

/// The one thing `shep daemon` can be asked to do rather than be.
///
/// A separate enum rather than a flag on [`DaemonArgs`] because it is a
/// different verb: `shep daemon` runs a shepherd in this process, and
/// `shep daemon reload` replaces the one already running with this
/// binary's own code.
#[derive(Debug, clap::Subcommand)]
pub enum DaemonCmd {
    /// Replace the running shepherd with this binary, and bring the flock back
    ///
    /// `cargo install shep` replaces the binary and leaves the shepherd
    /// running the old code. This is what restarts it, and it is the
    /// command a version-skew refusal names.
    Reload,
}

/// Arguments to `shep init`.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Where to write it. The extension picks the format: toml, yaml, yml,
    /// json or json5. Defaults to Flockfile.toml in this directory
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Show every option the grammar has, not just the common ones
    #[arg(long)]
    pub all: bool,
    /// Overwrite the Flockfile that is already here, keeping its own format
    #[arg(long)]
    pub force: bool,
}

/// Arguments to `shep runtime`.
#[derive(Debug, clap::Args)]
pub struct RuntimeArgs {
    /// Flockfile to run (default: discovered in the current directory)
    pub target: Option<String>,
    /// Run the supervisor in this process rather than splitting off an init.
    ///
    /// Set by the init half of a PID-1 split when it re-execs this binary,
    /// and never by a person. Also a safety catch: with this set the split
    /// cannot happen, so a mis-read pid can never produce a fork loop.
    #[arg(long, hide = true)]
    pub supervise: bool,
}

/// Arguments to `shep dev`.
///
/// No `--home` of its own, and the global one does not apply — see
/// `commands::dev::dev_home`.
#[derive(Debug, clap::Args)]
pub struct DevArgs {
    /// Script or Flockfile to run (default: discovered in this directory)
    pub target: Option<String>,
    /// Name for this sheep (script form only)
    #[arg(long)]
    pub name: Option<String>,
}

/// clap value parser over shep's own four boolean spellings — NOT clap's
/// `BoolishValueParser`, which also takes yes/no/y/n/on/off and would widen
/// the grammar on the flag side only.
fn bool_flag(value: &str) -> Result<bool, String> {
    shep_core::config::parse_daemon_bool(value)
        .ok_or_else(|| format!("expected one of 1, 0, true, false; got `{value}`"))
}

/// clap value parser over [`shep_core::config::LogLevel::from_name`] — the
/// same lowercase-only grammar `SHEP_LOG_LEVEL` accepts.
fn log_level_flag(value: &str) -> Result<shep_core::config::LogLevel, String> {
    shep_core::config::LogLevel::from_name(value).ok_or_else(|| {
        format!("expected one of off, error, warn, info, debug, trace; got `{value}`")
    })
}

/// clap value parser over `UpDuration`'s `FromStr`.
fn duration_flag(value: &str) -> Result<shep_core::values::UpDuration, String> {
    value
        .parse::<shep_core::values::UpDuration>()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn log_json_has_three_states() {
        use clap::Parser;
        let cases = [
            (vec!["shep", "daemon"], None),
            (vec!["shep", "daemon", "--log-json"], Some(true)),
            (vec!["shep", "daemon", "--log-json=false"], Some(false)),
            (vec!["shep", "daemon", "--log-json=1"], Some(true)),
        ];
        for (argv, expected) in cases {
            match Cli::try_parse_from(&argv).unwrap().command {
                Commands::Daemon(args) => assert_eq!(args.log_json, expected, "{argv:?}"),
                other => panic!("expected Daemon, got {other:?}"),
            }
        }
    }

    /// `shep daemon` with no subcommand is how this binary daemonizes:
    /// `launch::launch_daemon` re-execs it with exactly that one argument.
    /// An optional subcommand must not turn a bare invocation into a
    /// missing-subcommand error, or daemonization itself stops working and
    /// nothing in shep starts.
    #[test]
    fn a_bare_shep_daemon_still_boots_and_is_not_a_subcommand_error() {
        use clap::Parser;
        let parsed =
            Cli::try_parse_from(["shep", "daemon"]).expect("`shep daemon` must still parse");
        let Commands::Daemon(args) = parsed.command else {
            panic!("`shep daemon` must still parse as the daemon verb");
        };
        assert!(
            args.cmd.is_none(),
            "bare `shep daemon` must remain the boot path"
        );
    }

    /// fails if the subcommand changes what any existing `daemon` flag
    /// means. clap can behave surprisingly when a subcommand and a struct's
    /// own flags share one `Args` type, and every one of these flags is an
    /// init unit's `ExecStart` line somewhere.
    #[test]
    fn the_daemon_flags_still_parse_alongside_the_subcommand() {
        use clap::Parser;
        let argv = [
            "shep",
            "daemon",
            "--foreground",
            "--no-restore",
            "--log-json=false",
            "--log-level",
            "info",
            "--socket",
            "run/shep.sock",
            "--max-cron-sleep",
            "30s",
        ];
        let parsed = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed: {e}"));
        let Commands::Daemon(args) = parsed.command else {
            panic!("expected the daemon verb")
        };
        assert!(args.foreground);
        assert!(args.no_restore);
        assert_eq!(args.log_json, Some(false));
        assert_eq!(args.log_level, Some(shep_core::config::LogLevel::Info));
        assert_eq!(args.socket.as_deref(), Some(Path::new("run/shep.sock")));
        assert_eq!(
            args.max_cron_sleep
                .map(shep_core::values::UpDuration::as_duration),
            Some(std::time::Duration::from_secs(30))
        );
        assert!(
            args.cmd.is_none(),
            "flags alone must not select a subcommand"
        );
    }

    /// The verb the version-skew refusal names. It has to parse, or that
    /// refusal points an operator at a command that does not exist.
    #[test]
    fn daemon_reload_parses_as_the_reload_subcommand() {
        use clap::Parser;
        let parsed = Cli::try_parse_from(["shep", "daemon", "reload"])
            .expect("`shep daemon reload` must parse");
        let Commands::Daemon(args) = parsed.command else {
            panic!("expected the daemon verb")
        };
        assert!(
            matches!(args.cmd, Some(DaemonCmd::Reload)),
            "got {:?}",
            args.cmd
        );
    }

    /// fails if the flag grammar widens past the env grammar — the exact
    /// drift `parse_daemon_bool` exists to prevent.
    #[test]
    fn the_flag_bool_grammar_matches_the_env_grammar() {
        use clap::Parser;
        for wider in ["--log-json=yes", "--log-json=on", "--log-json=TRUE"] {
            assert!(
                Cli::try_parse_from(["shep", "daemon", wider]).is_err(),
                "{wider} must not parse"
            );
        }
    }

    /// fails if `runtime` stops parsing, or if `--supervise` becomes
    /// visible. It is the init's own re-exec flag; a person typing it
    /// should not find it in `--help`.
    #[test]
    fn runtime_parses_and_its_supervise_flag_is_hidden() {
        use clap::Parser;

        let bare = Cli::try_parse_from(["shep", "runtime"]).unwrap();
        let Commands::Runtime(args) = bare.command else {
            panic!("expected runtime")
        };
        assert_eq!(args.target, None, "no target means discover");
        assert!(!args.supervise, "a person never sets this");

        let with_target = Cli::try_parse_from(["shep", "runtime", "./Flockfile.toml"]).unwrap();
        let Commands::Runtime(args) = with_target.command else {
            panic!("expected runtime")
        };
        assert_eq!(args.target.as_deref(), Some("./Flockfile.toml"));

        let supervised = Cli::try_parse_from(["shep", "runtime", "--supervise"]).unwrap();
        let Commands::Runtime(args) = supervised.command else {
            panic!("expected runtime")
        };
        assert!(args.supervise, "the init passes --supervise to its child");

        use clap::CommandFactory;
        let cmd = Cli::command();
        let runtime = cmd.find_subcommand("runtime").unwrap();
        assert!(!runtime.is_hide_set(), "runtime is a real, documented verb");
        let supervise_arg = runtime
            .get_arguments()
            .find(|a| a.get_id().as_str() == "supervise")
            .expect("RuntimeArgs must still carry a hidden `supervise` field");
        assert!(
            supervise_arg.is_hide_set(),
            "--supervise must stay hidden from --help"
        );
    }

    #[test]
    fn start_takes_a_flockfile_flag_and_defaults_it_off() {
        use clap::Parser;
        let plain = Cli::try_parse_from(["shep", "start", "srv.js"]).unwrap();
        let flagged = Cli::try_parse_from(["shep", "start", "srv.js", "--flockfile"]).unwrap();
        match (plain.command, flagged.command) {
            (Commands::Start(a), Commands::Start(b)) => {
                assert!(!a.flockfile, "absent means script form");
                assert!(b.flockfile);
            }
            other => panic!("expected two Start commands, got {other:?}"),
        }
    }

    /// fails if a mode value does not reach `StartArgs.reset`, or if a
    /// target is silently absent from `--reset`.
    #[test]
    fn every_reset_mode_parses_from_its_argv_spelling() {
        use clap::Parser;

        fn parse_start(argv: &[&str]) -> StartArgs {
            match Cli::try_parse_from(argv).unwrap().command {
                Commands::Start(args) => args,
                other => panic!("expected start, got {other:?}"),
            }
        }

        assert_eq!(parse_start(&["shep", "start", "F.toml"]).reset, None);
        for (spelling, mode) in [
            ("file", ResetMode::File),
            ("policy", ResetMode::Policy),
            ("env", ResetMode::Env),
            ("all", ResetMode::All),
        ] {
            let flag = format!("--reset={spelling}");
            assert_eq!(
                parse_start(&["shep", "start", "F.toml", &flag]).reset,
                Some(mode),
                "argv spelling {spelling:?}"
            );
        }
    }

    /// fails if `--reset` with no value stops naming the four modes.
    /// Exact string: an error that lists three of four modes is worse than
    /// none. `value_enum` supplies this for free once `num_args = 0..=1`
    /// plus `default_missing_value` route a bare `--reset` through the
    /// same possible-values machinery as a typo, rather than through
    /// clap's unrelated "an equal sign is needed" message for a flag that
    /// takes no value at all.
    #[test]
    fn reset_with_no_value_is_a_usage_error_naming_every_mode() {
        use clap::Parser;

        let err = Cli::try_parse_from(["shep", "start", "F.toml", "--reset"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "error: a value is required for '--reset[=<RESET>]' but none was supplied\n  \
             [possible values: file, policy, env, all]\n\n\
             For more information, try '--help'.\n"
        );
    }

    /// fails if `StartArgs.targets`, a greedy variadic positional, ever
    /// swallows a mode meant for `--reset`, or a mode swallows a target.
    /// The equals form is required precisely to rule this out: the
    /// space-separated form is refused outright rather than resolved either
    /// way.
    #[test]
    fn a_reset_mode_does_not_swallow_the_target() {
        use clap::Parser;

        let parsed = Cli::try_parse_from(["shep", "start", "F.toml", "--reset=file"]).unwrap();
        match parsed.command {
            Commands::Start(args) => {
                assert_eq!(args.targets, vec!["F.toml".to_string()]);
                assert_eq!(args.reset, Some(ResetMode::File));
            }
            other => panic!("expected start, got {other:?}"),
        }

        // Mutation check: drop `require_equals` from the field and this
        // goes from `is_err()` to `is_ok()` -- clap resolves the
        // space-separated form instead of refusing it, which is exactly
        // the ambiguity this flag exists to rule out rather than resolve.
        assert!(
            Cli::try_parse_from(["shep", "start", "F.toml", "--reset", "file"]).is_err(),
            "the space-separated form must be refused, not resolved"
        );
    }

    /// fails if `--reset` is given a value none of the four modes claims.
    #[test]
    fn an_unknown_reset_mode_is_the_same_refusal_as_no_value() {
        use clap::Parser;

        let err = Cli::try_parse_from(["shep", "start", "F.toml", "--reset=banana"]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "error: invalid value 'banana' for '--reset[=<RESET>]'\n  \
             [possible values: file, policy, env, all]\n\n\
             For more information, try '--help'.\n"
        );
    }

    /// fails if serve stops binding loopback by default. Spec §10 fixes it and
    /// nothing else in the phase asserts it: delete `default_value` and clap
    /// requires the flag, write `Option<IpAddr>` instead and an unspecified
    /// default silently binds 0.0.0.0.
    #[test]
    fn serve_binds_loopback_on_port_8080_unless_told_otherwise() {
        use clap::Parser;
        use std::net::{IpAddr, Ipv4Addr};
        let cli = Cli::try_parse_from(["shep", "serve", "./x"]).unwrap();
        let Commands::Serve(args) = cli.command else {
            panic!("expected serve")
        };
        assert_eq!(args.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(args.port, 8080);
        assert!(!args.listing, "decision 9");
        assert!(!args.hidden, "decision 4");
        assert!(
            !args.follow_symlinks,
            "decision 5 — the refusal is the safe default"
        );
    }

    /// Pins the three facts `flock`'s own arguments settle: a bare listing
    /// does not follow, the interval defaults to the floor, and an interval
    /// without a follow is a usage error rather than a silent no-op.
    #[test]
    fn flock_follows_only_when_asked_and_defaults_to_a_one_second_interval() {
        use clap::Parser;

        let Commands::Flock(args) = Cli::try_parse_from(["shep", "flock"]).unwrap().command else {
            panic!("expected flock")
        };
        assert!(!args.follow, "a bare listing is still one moment");
        assert_eq!(args.interval, FOLLOW_INTERVAL_FLOOR_SECONDS);

        let Commands::Flock(args) =
            Cli::try_parse_from(["shep", "flock", "--follow", "--interval", "5"])
                .unwrap()
                .command
        else {
            panic!("expected flock")
        };
        assert!(args.follow);
        assert_eq!(args.interval, 5);

        assert!(
            Cli::try_parse_from(["shep", "flock", "--interval", "5"]).is_err(),
            "an interval with nothing to pace is a usage error"
        );
    }

    /// fails if the floor is dropped from `--interval`. Zero would spin the
    /// redraw against the shepherd as fast as the socket answers, and
    /// `sysinfo` reports no CPU at all over a window under 200 ms.
    #[test]
    fn a_zero_interval_is_refused() {
        use clap::Parser;

        assert!(Cli::try_parse_from(["shep", "flock", "--follow", "--interval", "0"]).is_err());
    }

    /// Pins [`DEFAULT_SELECTOR`] on both verbs that carry it.
    ///
    /// Fails if either loses its `default_value`: the bare invocation
    /// becomes a clap usage error instead of targeting the flock, which for
    /// `reopen` is the whole reason a signal — which carries no selector —
    /// can mean this verb at all. Both halves matter: an explicit selector
    /// must still win, or the default would be a hardcoded `all` wearing a
    /// default's clothes.
    #[test]
    fn bleats_and_reopen_default_to_every_sheep() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "reopen"]).unwrap().command;
        let Commands::Reopen(args) = bare else {
            panic!("`shep reopen` must parse with no selector")
        };
        assert_eq!(args.selector, "all");

        let bare = Cli::try_parse_from(["shep", "bleats"]).unwrap().command;
        let Commands::Bleats(args) = bare else {
            panic!("`shep bleats` must parse with no selector")
        };
        assert_eq!(args.selector, "all");

        let named = Cli::try_parse_from(["shep", "reopen", "web"])
            .unwrap()
            .command;
        let Commands::Reopen(args) = named else {
            panic!("expected reopen")
        };
        assert_eq!(args.selector, "web");
    }

    /// fails if clap accepts `shep stock web 0`. The refusal exists daemon-side
    /// too, and deliberately in both places — but a usage error should not cost a
    /// connection, and `range(1..)` is what puts the accepted range into `--help`.
    #[test]
    fn stock_refuses_a_count_of_zero_before_it_reaches_the_wire() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "stock", "web", "0"]).is_err());
        assert!(Cli::try_parse_from(["shep", "stock", "web", "1"]).is_ok());
    }

    /// fails if `stock` grows a default target. `shep stock 4` must be a usage
    /// error, never "stock whatever app happens to be first".
    #[test]
    fn stock_requires_both_the_name_and_the_count() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "stock"]).is_err());
        assert!(Cli::try_parse_from(["shep", "stock", "web"]).is_err());
    }

    /// Every verb sharing [`SelectorArgs`] must refuse to run without one.
    ///
    /// Fails if that struct's `selector` field ever gains a `default_value`.
    /// That is a one-line edit reaching six verbs at once, and it is worth a
    /// case of its own precisely because it looks harmless: it does not break
    /// a single other test, and what it changes is that `shep stop` — typed
    /// by an operator who then remembered which sheep they meant — becomes
    /// `shep stop all` instead of a usage error. `reopen` and `bleats`
    /// deliberately do have that default (see
    /// [`bleats_and_reopen_default_to_every_sheep`]); the difference is that
    /// neither of them ends a process.
    ///
    /// The explicit form is asserted alongside, for the reason
    /// [`flush_refuses_to_run_without_a_selector`] gives: a verb that had
    /// stopped accepting any selector at all would pass the first half on its
    /// own.
    ///
    /// `trigger` joins this group too, but cannot share the loop above
    /// verbatim: [`TriggerArgs`] carries two required positionals, not one,
    /// so a bare `shep trigger web` (selector only) is already a usage error
    /// for missing `action` regardless of whether `selector` itself has a
    /// default — that loop's second assertion would pass by accident. What
    /// pins `selector` specifically is the same thing
    /// `home_flag_is_wired_to_the_shep_home_env_var` checks for `--home`:
    /// the clap `Arg` itself, read directly off `trigger`'s own `Command`.
    #[test]
    fn a_selector_taking_verb_refuses_to_run_without_one() {
        use clap::{CommandFactory, Parser};
        for verb in [
            "stop", "restart", "reload", "delete", "describe", "thatlldo",
        ] {
            assert!(
                Cli::try_parse_from(["shep", verb]).is_err(),
                "`shep {verb}` with no selector must be a usage error, never \
                 the whole flock"
            );
            assert!(
                Cli::try_parse_from(["shep", verb, "web"]).is_ok(),
                "`shep {verb} web` must still parse"
            );
        }

        assert!(
            Cli::try_parse_from(["shep", "trigger"]).is_err(),
            "`shep trigger` with neither selector nor action must be a usage error"
        );
        assert!(
            Cli::try_parse_from(["shep", "trigger", "web", "reload-config"]).is_ok(),
            "`shep trigger web reload-config` (selector, then action) must parse"
        );

        let cmd = Cli::command();
        let trigger = cmd.find_subcommand("trigger").unwrap();
        let selector_arg = trigger
            .get_arguments()
            .find(|a| a.get_id().as_str() == "selector")
            .expect("TriggerArgs must still carry a `selector` field");
        assert!(
            selector_arg.is_required_set(),
            "trigger's selector must stay required, never default to the whole flock"
        );
    }

    /// The other side of [`bleats_and_reopen_default_to_every_sheep`]: the
    /// log-plane verb that destroys data must NOT have a default.
    ///
    /// Fails if `flush` is ever given a `default_value`, or moved onto
    /// [`ReopenArgs`] — either of which turns a bare `shep flush`, the single
    /// most likely slip of the finger this CLI offers, from a usage error
    /// into "empty every log file in the flock" with nothing to undo it. The
    /// explicit form is asserted alongside, so a verb that rejected every
    /// selector could not pass the first half alone.
    ///
    /// `required_unless_present = "daemon"` is what keeps the first half true
    /// now that the selector is an `Option`: without it, a bare `shep flush`
    /// parses to `selector: None` and reaches the handler.
    #[test]
    fn flush_refuses_to_run_without_a_selector() {
        use clap::Parser;
        assert!(
            Cli::try_parse_from(["shep", "flush"]).is_err(),
            "`shep flush` with no selector must be a usage error, never the \
             whole flock"
        );

        let named = Cli::try_parse_from(["shep", "flush", "all"])
            .unwrap()
            .command;
        let Commands::Flush(args) = named else {
            panic!("expected flush")
        };
        assert_eq!(args.selector.as_deref(), Some("all"));
        assert!(
            !args.daemon,
            "a plain flush must not reach the shepherd's own logs"
        );
    }

    /// Fails if `--daemon` stops replacing the selector — either by gaining a
    /// selector of its own (the bare form stops parsing) or by losing
    /// `conflicts_with` (the combined form starts parsing).
    ///
    /// Both halves are the decision [`FlushArgs`]'s doc argues for. The bare
    /// form must work, because it is the only spelling of "empty the
    /// shepherd's own logs" and requiring a sheep selector alongside it would
    /// be nonsense. The combined form must NOT, because an operator typing
    /// `shep flush all --daemon` out of habit is exactly the accident that
    /// keeping the two targets apart exists to prevent.
    #[test]
    fn the_daemon_flag_replaces_the_selector_rather_than_riding_along_with_it() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "flush", "--daemon"])
            .expect("`shep flush --daemon` is the only spelling there is")
            .command;
        let Commands::Flush(args) = bare else {
            panic!("expected flush")
        };
        assert!(args.daemon);
        assert_eq!(args.selector, None);

        assert!(
            Cli::try_parse_from(["shep", "flush", "all", "--daemon"]).is_err(),
            "the shepherd's own logs are a separate act, never a rider on a \
             flock-wide flush"
        );
    }

    /// `shep barks` takes no selector and defaults `--tail` to `None` (every
    /// bark); `--tail N` parses to `Some(N)`. Fails if either the bare form
    /// stops parsing or `--tail` stops being optional — a `default_value`
    /// on it would turn "show everything" into a silent 10-line window with
    /// nothing to name why.
    #[test]
    fn barks_takes_no_selector_and_tail_defaults_to_everything() {
        use clap::Parser;
        let bare = Cli::try_parse_from(["shep", "barks"]).unwrap().command;
        let Commands::Barks(args) = bare else {
            panic!("`shep barks` must parse with no selector")
        };
        assert_eq!(args.tail, None);

        let tailed = Cli::try_parse_from(["shep", "barks", "--tail", "20"])
            .unwrap()
            .command;
        let Commands::Barks(args) = tailed else {
            panic!("expected barks")
        };
        assert_eq!(args.tail, Some(20));
    }

    /// fails if `shep unset` with no key and no --all is accepted. It would have to
    /// mean either nothing or everything, and the everything reading is
    /// unrecoverable.
    #[test]
    fn unset_needs_a_key_or_the_all_flag() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "unset"]).is_err());
        assert!(Cli::try_parse_from(["shep", "unset", "a"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "unset", "--all"]).is_ok());
    }

    /// fails if `--all` composes with a key. `shep unset a --all` would be an
    /// operator asking for one thing and a flag doing something far larger —
    /// the same conflict `shep flush all --daemon` is a usage error for.
    #[test]
    fn unset_refuses_a_key_and_all_together() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "unset", "a", "--all"]).is_err());
    }

    /// fails if `shep get` starts requiring a key. Bare `get` listing the whole
    /// store is the discovery path — an operator who does not remember what they
    /// set has nowhere else to look.
    #[test]
    fn get_takes_an_optional_key() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "get"]).is_ok());
        assert!(Cli::try_parse_from(["shep", "get", "a"]).is_ok());
    }

    /// fails if `set` becomes anything but two required positionals. A `set` with a
    /// defaultable value would let `shep set a` silently store an empty string.
    #[test]
    fn set_needs_both_a_key_and_a_value() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "set", "a"]).is_err());
        assert!(Cli::try_parse_from(["shep", "set", "a", "1"]).is_ok());
    }

    #[test]
    fn format_defaults_to_table_and_accepts_json() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["shep", "flock"]).unwrap();
        assert_eq!(cli.global.format, Format::Table);
        let cli = Cli::try_parse_from(["shep", "--format", "json", "flock"]).unwrap();
        assert_eq!(cli.global.format, Format::Json);
    }

    /// fails if `--style` stops being optional (a run must still work with
    /// nothing said, falling through to `$SHEP_STYLE`/`shep.toml`/default —
    /// see `style::resolve`), or if a level clap now rejects or mis-parses.
    #[test]
    fn style_flag_defaults_to_unset_and_accepts_the_three_levels() {
        use crate::style::StyleLevel;
        use clap::Parser;

        let cli = Cli::try_parse_from(["shep", "flock"]).unwrap();
        assert_eq!(cli.global.style, None);

        for (raw, expected) in [
            ("full", StyleLevel::Full),
            ("plain", StyleLevel::Plain),
            ("bare", StyleLevel::Bare),
        ] {
            let cli = Cli::try_parse_from(["shep", "--style", raw, "flock"]).unwrap();
            assert_eq!(cli.global.style, Some(expected), "--style {raw}");
        }

        assert!(Cli::try_parse_from(["shep", "--style", "loud", "flock"]).is_err());
    }

    /// fails if `StyleArgs::level` reverts to `Option<String>` (the
    /// original defect: a value that parsed but was read by nothing), or
    /// if the verb's grammar ever drifts from `--style`'s -- a value
    /// clap accepts on one spelling and rejects on the other would leave
    /// an operator unable to guess which one is broken.
    #[test]
    fn style_verb_parses_the_same_grammar_as_the_style_flag() {
        use crate::style::StyleLevel;
        use clap::Parser;

        let cli = Cli::try_parse_from(["shep", "style"]).unwrap();
        match cli.command {
            Commands::Style(args) => {
                assert_eq!(args.level, None, "bare `shep style` still reports")
            }
            other => panic!("expected Style, got {other:?}"),
        }

        for (raw, expected) in [
            ("full", StyleLevel::Full),
            ("plain", StyleLevel::Plain),
            ("bare", StyleLevel::Bare),
        ] {
            let cli = Cli::try_parse_from(["shep", "style", raw]).unwrap();
            match cli.command {
                Commands::Style(args) => assert_eq!(args.level, Some(expected), "style {raw}"),
                other => panic!("expected Style, got {other:?}"),
            }
        }

        let bad_flag = Cli::try_parse_from(["shep", "--style", "loud", "flock"]).unwrap_err();
        let bad_verb = Cli::try_parse_from(["shep", "style", "loud"]).unwrap_err();
        assert_eq!(
            bad_flag.kind(),
            bad_verb.kind(),
            "a bad value fails the same way through either spelling"
        );
    }

    /// `std::env::set_var` is `unsafe` in edition 2024 and this crate is
    /// `#![forbid(unsafe_code)]`, so nothing here can establish an ambient
    /// `$SHEP_HOME` and observe clap actually reading it. The next best
    /// thing, and the thing that actually matters for `$SHEP_HOME` to keep
    /// working, is pinning that clap was *told* to read it: if `env =
    /// "SHEP_HOME"` (`cli.rs:30`) is ever deleted, this fails.
    #[test]
    fn home_flag_is_wired_to_the_shep_home_env_var() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let home_arg = cmd
            .get_arguments()
            .find(|a| a.get_id().as_str() == "home")
            .expect("GlobalArgs::home must still be a flattened argument named `home`");
        assert_eq!(home_arg.get_env(), Some(std::ffi::OsStr::new("SHEP_HOME")));
    }

    /// fails if `Commands::Dog` is wired to another verb, or if it is not
    /// hidden. It is a re-exec target, not something an operator runs.
    #[test]
    fn the_dog_subcommand_parses_and_stays_hidden() {
        use clap::{CommandFactory, Parser};

        let parsed = Cli::try_parse_from(["shep", "dog", "metrics"])
            .unwrap()
            .command;
        let Commands::Dog(args) = parsed else {
            panic!("expected dog")
        };
        assert_eq!(args.name, "metrics");

        let cmd = Cli::command();
        assert!(
            cmd.find_subcommand("dog").unwrap().is_hide_set(),
            "dog must stay hidden from --help"
        );
    }

    /// Fails if `enable`'s pm2-spelled `--exec` alias loses its `hide =
    /// true` and starts teaching itself in `--help` — the whole reason it
    /// is an argument-level hide rather than a documented flag: `shep
    /// adopt` is the verb the help text should point an operator at.
    #[test]
    fn the_exec_alias_stays_hidden_from_help() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let enable = cmd.find_subcommand("enable").unwrap();
        let exec_arg = enable
            .get_arguments()
            .find(|a| a.get_id().as_str() == "exec")
            .expect("EnableArgs must still carry a hidden `exec` field");
        assert!(
            exec_arg.is_hide_set(),
            "--exec must stay hidden from --help"
        );
    }

    /// fails if the control gate stops being on by default, or stops being
    /// closeable from the flag.
    #[test]
    fn actions_are_on_unless_read_only_says_otherwise() {
        use clap::Parser;
        let Commands::Lookout(default) = Cli::try_parse_from(["shep", "lookout"]).unwrap().command
        else {
            panic!("lookout parses to its own variant")
        };
        assert!(!default.read_only);

        let Commands::Lookout(flagged) = Cli::try_parse_from(["shep", "lookout", "--read-only"])
            .unwrap()
            .command
        else {
            panic!("lookout parses to its own variant")
        };
        assert!(flagged.read_only);
    }

    /// fails if `--allow-control` starts parsing again: lookout takes no
    /// such flag.
    #[test]
    fn allow_control_no_longer_parses() {
        use clap::Parser;
        assert!(Cli::try_parse_from(["shep", "lookout", "--allow-control"]).is_err());
    }

    /// fails if the derive comes back on either type: `shep secret set`
    /// carries the operator's plaintext value, and a `{:?}` anywhere in the
    /// binary would print it (IR-41). Exact strings, not a `contains`, so a
    /// derive cannot pass by happening to omit this one value.
    #[test]
    fn secret_command_debug_does_not_leak() {
        use clap::Parser;
        let Commands::Secret(args) = Cli::try_parse_from([
            "shep",
            "secret",
            "set",
            "DB_PASSWORD",
            "hunter2",
            "--env",
            "staging",
        ])
        .unwrap()
        .command
        else {
            panic!("secret parses to its own variant")
        };

        assert_eq!(
            format!("{args:?}"),
            "SecretArgs { command: Set { key: \"DB_PASSWORD\", value: Some(<7 bytes>), \
             env: Some(\"staging\"), stdin: false } }"
        );
        assert_eq!(
            format!("{:?}", SecretCommand::List),
            "List",
            "a variant carrying no value still has to render"
        );
    }
}
