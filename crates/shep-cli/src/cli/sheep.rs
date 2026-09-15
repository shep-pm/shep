//! Arguments for the verbs that act on sheep: registering them, running
//! them, ending them, and talking to a running one.
//!
//! [`SelectorArgs`] is the shape most of them share, and the docs on the
//! four that do not share it say why in each case. That is the whole reason
//! these structs sit together: each one is an argument for or against the
//! common selector grammar, and the argument only reads as one if they are
//! in one place.

use std::net::IpAddr;
use std::path::PathBuf;

use shep_core::config::ResetDepth;

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
/// ([`FlushArgs`](crate::cli::FlushArgs)) only because it has a second target that is not a
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
    /// One or more: name, id, `name:slot`, `all`, `api-*`, `/regex/`, `fold:<name>`
    ///
    /// Several are applied in turn, not atomically: `shep stop a b c` where
    /// `b` matches nothing still stops `a` and `c`, and the exit code is the
    /// first failure.
    ///
    /// A pattern carrying `*`, `?`, `[` or `{` is a glob, anchored, so
    /// `api-*` selects `api-auth` and not `my-api-auth`. Quote it: your
    /// shell expands `api-*` against filenames first, and zsh refuses
    /// outright when none match. A name with no such character is exact, so
    /// `web.1` is the sheep called `web.1`.
    #[arg(required = true, num_args = 1..)]
    pub selectors: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands};

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
}
