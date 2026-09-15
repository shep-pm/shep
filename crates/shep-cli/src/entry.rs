//! Argument routing: building an alias binary's argv and running the
//! parsed result on a fresh runtime.

use std::ffi::OsString;
use std::io::IsTerminal;

use clap::Parser;
use cli::Cli;

use crate::adopted::dispatch_adopted_dog;
use crate::config::{must_render_bare, resolve_style};
use crate::dispatch::run;
use crate::home::print_shepherd_status;
use crate::{cli, exit, lookout, output, style};
use exit::ExitCode;

/// Builds the argument vector an alias binary should be parsed as: `verb`
/// inserted after `argv[0]`.
///
/// `daemon` and `dog` pass through untouched. The supervisor spawns those two
/// as `std::env::current_exe()` plus the verb, and under an alias binary
/// `current_exe()` is `shep-runtime`, so inserting a verb would turn
/// `shep-runtime dog metrics` into `shep runtime dog metrics`.
pub(crate) fn alias_argv(verb: &str, mut argv: Vec<OsString>) -> Vec<OsString> {
    let passthrough = matches!(
        argv.get(1).and_then(|arg| arg.to_str()),
        Some("daemon" | "dog")
    );
    if !passthrough {
        argv.insert(1, OsString::from(verb));
    }
    argv
}

/// Parses `argv` and runs it on a fresh multi-threaded runtime.
pub(crate) fn run_argv(argv: Vec<OsString>) -> std::process::ExitCode {
    // Env-gated hook for `tests/term_panic_order.rs`, not a clap variant, so
    // it carries no `--help` entry.
    if std::env::var_os("SHEP_TERM_PANIC_PROBE").is_some() {
        lookout::term::probe_panic_for_test();
    }
    // `try_parse_from`, not `parse_from`: the latter prints and exits inside
    // clap, so bare `shep` and `shep help` could not carry a status line.
    let parsed = Cli::try_parse_from(argv.clone());
    // Before clap renders its own "unrecognized subcommand" error, check
    // whether the token it could not place names an adopted dog.
    if let Err(ref err) = parsed
        && let Some(code) = dispatch_adopted_dog(&argv, err)
    {
        return code;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("shep: could not start an async runtime: {err}");
            return std::process::ExitCode::from(ExitCode::Failure as u8);
        }
    };
    let cli = match parsed {
        Ok(cli) => cli,
        Err(err) => {
            #[cfg(unix)]
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::MissingSubcommand
            ) {
                runtime.block_on(print_shepherd_status(&argv));
            }
            // clap renders the help or the usage error and picks the exit
            // code, as `parse_from` would have.
            err.exit();
        }
    };
    // What is configured and whether the hard rule overrides it stay two
    // steps: `style_source` rides down to `lookout::lookout`, whose settings
    // screen reports which layer won.
    let (configured, style_source) = resolve_style(&cli.global);
    let level = if must_render_bare(std::io::stdout().is_terminal(), cli.global.format) {
        style::StyleLevel::Bare
    } else {
        configured
    };
    // Every terminal fact is read at this seam and nowhere else: `NO_COLOR`,
    // `$TERM`, `$COLORTERM` and the width.
    let style = style::Presentation::new(
        level,
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("COLORTERM").as_deref(),
        output::terminal_width(),
    );
    std::process::ExitCode::from(
        runtime.block_on(run(cli, style, (configured, style_source))) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn an_alias_supplies_its_verb() {
        let argv = alias_argv(
            "runtime",
            vec!["shep-runtime".into(), "./Flockfile.toml".into()],
        );
        assert_eq!(
            argv,
            vec![
                OsString::from("shep-runtime"),
                OsString::from("runtime"),
                OsString::from("./Flockfile.toml"),
            ]
        );
    }

    /// `shep-dev` on its own must be `shep dev`, not `shep`.
    #[test]
    fn an_alias_with_no_arguments_still_supplies_its_verb() {
        let argv = alias_argv("dev", vec!["shep-dev".into()]);
        assert_eq!(
            argv,
            vec![OsString::from("shep-dev"), OsString::from("dev")]
        );
    }

    /// `shep_daemon::dogs` spawns a built-in dog as `current_exe() dog
    /// <name>`, which under `shep-runtime` is this argument vector.
    #[test]
    fn an_alias_passes_the_two_re_exec_verbs_through_untouched() {
        for verb in ["daemon", "dog"] {
            let argv = alias_argv(
                "runtime",
                vec!["shep-runtime".into(), verb.into(), "metrics".into()],
            );
            assert_eq!(
                argv[1],
                OsString::from(verb),
                "{verb} must not be rewritten"
            );
            assert_eq!(argv.len(), 3, "{verb}: nothing may be inserted");
        }
    }

    /// The pass-through is an exact match, not a prefix: a sheep named
    /// `dogfood` must still reach `runtime`.
    #[test]
    fn the_pass_through_matches_the_whole_argument_and_not_a_prefix() {
        let argv = alias_argv("runtime", vec!["shep-runtime".into(), "dogfood".into()]);
        assert_eq!(argv[1], OsString::from("runtime"));
        assert_eq!(argv[2], OsString::from("dogfood"));
    }

    /// A well-formed alias vector must still reach the verb: a `runtime`
    /// subcommand taking a required positional would not.
    #[test]
    fn the_alias_vector_parses_to_the_expected_command() {
        use clap::Parser;
        use cli::Commands;
        let argv = alias_argv(
            "dog",
            vec!["shep-runtime".into(), "dog".into(), "metrics".into()],
        );
        let cli = Cli::try_parse_from(argv).expect("the passthrough vector must parse");
        assert!(matches!(cli.command, Commands::Dog(_)));
    }

    /// `--supervise` is the init's own re-exec.
    #[test]
    fn the_runtime_alias_vector_parses_to_the_runtime_command() {
        use clap::Parser;
        use cli::Commands;
        let argv = alias_argv("runtime", vec!["shep-runtime".into(), "--supervise".into()]);
        let cli = Cli::try_parse_from(argv).unwrap();
        let Commands::Runtime(args) = cli.command else {
            panic!("expected runtime")
        };
        assert!(args.supervise);
    }

    /// Without `propagate_version` the alias binaries have no working
    /// `--version`: `shep-runtime --version` parses as `shep runtime
    /// --version`.
    #[test]
    fn a_subcommand_answers_version() {
        use clap::Parser;
        let err = Cli::try_parse_from(["shep", "dogs", "--version"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn save_parses_to_its_own_command() {
        use clap::Parser;
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "save"]).unwrap().command,
            Commands::Save
        ));
    }

    #[test]
    fn dogs_parses_to_its_own_command() {
        use clap::Parser;
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "dogs"]).unwrap().command,
            Commands::Dogs(_)
        ));
    }

    #[test]
    fn dogs_available_parses_with_its_filter() {
        use clap::Parser;
        use cli::Commands;
        let parsed = Cli::try_parse_from(["shep", "dogs", "--available", "spot"])
            .unwrap()
            .command;
        let Commands::Dogs(args) = parsed else {
            panic!("expected dogs")
        };
        assert!(args.available);
        assert_eq!(args.filter.as_deref(), Some("spot"));
    }

    /// `DogArgs` is outside the `SelectorArgs` family, so `cli.rs`'s
    /// requiredness check does not reach its `name` positional.
    #[test]
    fn enable_and_disable_parse_to_their_own_commands_and_require_a_name() {
        use clap::Parser;
        use cli::Commands;

        let enabled = Cli::try_parse_from(["shep", "enable", "metrics"])
            .unwrap()
            .command;
        let Commands::Enable(args) = enabled else {
            panic!("expected enable")
        };
        assert_eq!(args.name, "metrics");

        let disabled = Cli::try_parse_from(["shep", "disable", "metrics"])
            .unwrap()
            .command;
        let Commands::Disable(args) = disabled else {
            panic!("expected disable")
        };
        assert_eq!(args.name, "metrics");

        assert!(
            Cli::try_parse_from(["shep", "enable"]).is_err(),
            "`shep enable` with no name must be a usage error"
        );
        assert!(
            Cli::try_parse_from(["shep", "disable"]).is_err(),
            "`shep disable` with no name must be a usage error"
        );
    }

    /// `adopt` needs only `path`; `name` is an optional `--name` flag.
    /// `rehome` shares `DogArgs` with `disable`, so it needs only the name.
    #[test]
    fn adopt_and_rehome_parse_to_their_own_commands_and_require_their_arguments() {
        use clap::Parser;
        use cli::Commands;

        let adopted =
            Cli::try_parse_from(["shep", "adopt", "/opt/bin/shep-otel", "--name", "otel"])
                .unwrap()
                .command;
        let Commands::Adopt(args) = adopted else {
            panic!("expected adopt")
        };
        assert_eq!(args.path, PathBuf::from("/opt/bin/shep-otel"));
        assert_eq!(args.name, Some("otel".to_string()));

        // `--name` is optional: a bare path still parses, with no name.
        let unnamed = Cli::try_parse_from(["shep", "adopt", "/opt/bin/shep-otel"])
            .unwrap()
            .command;
        let Commands::Adopt(args) = unnamed else {
            panic!("expected adopt")
        };
        assert_eq!(args.path, PathBuf::from("/opt/bin/shep-otel"));
        assert_eq!(args.name, None);

        let rehomed = Cli::try_parse_from(["shep", "rehome", "otel"])
            .unwrap()
            .command;
        let Commands::Rehome(args) = rehomed else {
            panic!("expected rehome")
        };
        assert_eq!(args.name, "otel");

        assert!(
            Cli::try_parse_from(["shep", "adopt"]).is_err(),
            "`shep adopt` with no path must be a usage error"
        );
        assert!(
            Cli::try_parse_from(["shep", "rehome"]).is_err(),
            "`shep rehome` with no name must be a usage error"
        );
    }

    /// `--exec`'s value and `enable`'s positional `name` are both strings, so
    /// the two landing in the wrong `AdoptArgs` fields is otherwise silent.
    #[test]
    fn the_hidden_pm2_spelling_reaches_adopt_with_the_arguments_the_right_way_round() {
        use clap::Parser;
        use cli::Commands;

        let parsed = Cli::try_parse_from(["shep", "enable", "--exec", "/opt/bin/d", "otel"])
            .unwrap()
            .command;
        let Commands::Enable(args) = parsed else {
            panic!("expected enable")
        };
        assert_eq!(args.name, "otel");
        assert_eq!(args.exec, Some(PathBuf::from("/opt/bin/d")));

        // A plain `enable` carries no path, the branch the dispatch reads
        // to decide `enable` against `adopt`.
        let plain = Cli::try_parse_from(["shep", "enable", "metrics"])
            .unwrap()
            .command;
        let Commands::Enable(args) = plain else {
            panic!("expected enable")
        };
        assert_eq!(args.exec, None);
    }

    #[test]
    fn muster_parses_to_its_own_command() {
        use clap::Parser;
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "muster"]).unwrap().command,
            Commands::Muster
        ));
    }

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

    /// The `env` half of the same parse gate, plus the two arguments that
    /// decide where a value goes.
    #[test]
    fn import_env_parses_its_file_app_and_secret_patterns() {
        use clap::Parser;
        use cli::{Commands, ImportCommand};
        let cli = Cli::try_parse_from([
            "shep", "import", "env", "app.env", "--app", "web", "--secret", "DB_*",
        ])
        .unwrap();
        let Commands::Import(args) = cli.command else {
            panic!("`shep import env` did not reach the import verb");
        };
        let ImportCommand::Env(args) = args.command else {
            panic!("`shep import env` did not reach its own subcommand");
        };
        assert_eq!(args.file.to_str(), Some("app.env"));
        assert_eq!(args.app, "web");
        assert_eq!(args.secret, ["DB_*"]);
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

    /// Pins clap's parse only; `cli_e2e.rs`'s
    /// `barks_reads_the_history_with_no_shepherd_running` proves the arm
    /// reaches `dogs::barks`.
    #[test]
    fn barks_parses_to_its_own_command() {
        use clap::Parser;
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "barks"]).unwrap().command,
            Commands::Barks(_)
        ));
    }

    /// Pins clap's parse only; the dispatch arms are covered below.
    #[test]
    fn startup_and_unstartup_parse_to_their_own_commands() {
        use clap::Parser;
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "startup"]).unwrap().command,
            Commands::Startup(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["shep", "unstartup"]).unwrap().command,
            Commands::Unstartup(_)
        ));
        let named = Cli::try_parse_from(["shep", "startup", "--user", "deploy"])
            .unwrap()
            .command;
        let Commands::Startup(args) = named else {
            panic!("expected startup")
        };
        assert_eq!(args.user.as_deref(), Some("deploy"));
    }

    /// `resurrect` exists for a pm2 muscle-memory invocation, so it must
    /// reach `muster` and stay out of `--help`.
    #[test]
    fn resurrect_is_a_hidden_alias_for_muster() {
        use clap::{CommandFactory, Parser};
        use cli::Commands;
        assert!(matches!(
            Cli::try_parse_from(["shep", "resurrect"]).unwrap().command,
            Commands::Muster
        ));
        let cmd = Cli::command();
        let muster = cmd.find_subcommand("muster").unwrap();
        assert!(
            muster.get_visible_aliases().next().is_none(),
            "resurrect must stay out of --help"
        );
    }
}
