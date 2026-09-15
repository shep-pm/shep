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

mod flock;
mod help;
mod logs;
mod sheep;
mod store;
mod verbs;

use std::path::PathBuf;

use help::{HELP_TEMPLATE, version_text};

pub use flock::{FlockArgs, FoldArgs, LookoutArgs};
pub use logs::{BarksArgs, BleatsArgs, FlushArgs, ReopenArgs};
// Two defaults whose only caller outside their own module is a test
// elsewhere in the crate: lib.rs builds a `FlockArgs` by hand, and
// commands/bleats.rs builds a `BleatsArgs`. A release build reaches neither
// through this path, which is what the allow is for.
#[allow(unused_imports)]
pub(crate) use flock::FOLLOW_INTERVAL_FLOOR_SECONDS;
#[allow(unused_imports)]
pub use logs::DEFAULT_BLEAT_LINES;
pub use store::{KvGetArgs, KvSetArgs, KvUnsetArgs, SecretArgs, SecretCommand};
pub use sheep::{
    ResetMode, SelectorArgs, ServeArgs, SignalArgs, StartArgs, StockArgs, TriggerArgs, WhisperArgs,
};
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
}
