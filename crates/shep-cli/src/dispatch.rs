//! Parses no argv of its own: [`run`] maps an already-parsed [`Cli`] to the
//! verb's own module, the crate's single largest dispatch.

use std::io::{IsTerminal, Write};

use cli::{AdoptArgs, Cli, Commands, Format, ImportCommand};
use commands::admin;
use commands::bleats;
use commands::dev;
use commands::dogs;
use commands::import;
use commands::kv;
use commands::lifecycle;
use commands::lifecycle::Load;
use commands::logs;
use commands::muster;
use commands::query;
use commands::runtime;
use commands::schema;
use commands::secret;
// aliased: `commands::serve` would collide with the crate-root `serve` module
use commands::serve::serve as serve_command;
use commands::shep_toml::{ShepToml, ShepTomlError};
use commands::signal;
#[cfg(unix)]
use commands::startup;
use commands::trigger;
use commands::whisper;
use exit::ExitCode;
use output::Streams;

use crate::client::{connect_or_spawn_client, load_command, run_daemon_command};
use crate::commands::init;
use crate::config::{resolve_style, style_write_is_overridden};
use crate::home::{ensure_home, report_home_refusal, resolve_paths, scaffold_first_run_interpreters};
use crate::version_guard::{VersionGuard, connect_client, flock_command};
use crate::{cli, commands, completions, dog, exit, lookout, output, status, style, welcome, whistle};

/// Parses, resolves `$SHEP_HOME` for the verbs that need it, and dispatches
/// to the verb's own module.
///
/// Every command receives an already-connected client; no verb module
/// connects or autostarts. `Start` and `Muster` are the only two arms that
/// bring a shepherd up, through [`connect_or_spawn_client`].
///
/// `Startup` and `Unstartup` skip the shared `$SHEP_HOME` gate below: with
/// no `--home`/`$SHEP_HOME` the unit's home is the TARGET user's passwd
/// home, which the gate would get wrong under `sudo`, and a named one goes
/// through [`ensure_home`] inside the `Startup` arm. `unstartup` ignores
/// `--home` entirely, since a removal is addressed by the unit's path and
/// label alone. `style` is already forced to [`style::StyleLevel::Bare`] if
/// the hard rule applies; `resolved_style` is the unforced pair
/// `Commands::Style` and the lookout settings screen read.
pub(crate) async fn run(
    cli: Cli,
    style: style::Presentation,
    resolved_style: (style::StyleLevel, style::StyleSource),
) -> ExitCode {
    let fmt = cli.global.format;
    // Resolved once, here: the dispatch below partially moves `cli.command`,
    // so no arm can borrow it to ask this question for itself.
    let guard = VersionGuard::for_command(&cli.command);

    // `StdoutLock`/`StderrLock` are process-wide, so the locked pair further
    // down is right only for a verb that finishes in milliseconds. A guard
    // held on the main thread for a process lifetime blocks the first record
    // any other thread writes, forever, and wedges that task.
    match cli.command {
        Commands::Completions(ref args) => {
            // Status to stderr: the script on stdout is meant to be sourced,
            // and a status line in it would be executed.
            if let Ok(paths) = resolve_paths(&cli.global) {
                let shepherd = status::ShepherdStatus::probe(&paths).await;
                if std::io::stderr().is_terminal() {
                    let mut err = std::io::stderr();
                    let _ = writeln!(err, "{}", status::one_line(&shepherd));
                }
            }
            let mut out = std::io::stdout().lock();
            return completions::completions(&mut out, args);
        }
        Commands::Daemon(ref args) => {
            // `daemon reload` stops a shepherd and starts one rather than
            // being one. Unlocked handles: a teardown ladder plus a boot
            // takes seconds.
            if let Some(cli::DaemonCmd::Reload) = args.cmd {
                let paths = match resolve_paths(&cli.global) {
                    Ok(paths) => paths,
                    Err(refusal) => return report_home_refusal(fmt, &refusal),
                };
                let mut out = std::io::stdout();
                let mut err = std::io::stderr();
                let mut streams = Streams {
                    out: &mut out,
                    err: &mut err,
                    style,
                    fmt,
                };
                return commands::daemon::reload(&mut streams, &paths, guard).await;
            }
            return run_daemon_command(fmt, &cli.global, args).await;
        }
        // A named home goes through the shared gate: refused if missing,
        // never created. With none, the startup module resolves the target
        // user's `<passwd home>/.shep`; `ensure_home` would read this
        // process's `$HOME`, which `sudo` sets to root's.
        Commands::Startup(ref args) => {
            #[cfg(windows)]
            let _ = args;
            let named_home = if cli.global.home.is_some() {
                match ensure_home(&cli.global) {
                    Ok((paths, _)) => Some(paths.home),
                    Err(refusal) => return report_home_refusal(fmt, &refusal),
                }
            } else {
                None
            };
            #[cfg(windows)]
            let _ = named_home;
            let mut out = std::io::stdout().lock();
            let mut err = std::io::stderr().lock();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style,
                fmt,
            };
            #[cfg(unix)]
            return startup::startup(&mut streams, named_home.as_deref(), args);
            #[cfg(windows)]
            return streams.fail(ExitCode::Failure, WINDOWS_NO_SERVICE);
        }
        Commands::Unstartup(ref args) => {
            #[cfg(windows)]
            let _ = args;
            let mut out = std::io::stdout().lock();
            let mut err = std::io::stderr().lock();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style,
                fmt,
            };
            #[cfg(unix)]
            return startup::unstartup(&mut streams, args);
            #[cfg(windows)]
            return streams.fail(ExitCode::Failure, WINDOWS_NO_SERVICE);
        }
        // Needs no `$SHEP_HOME` at all, like `Completions` above.
        Commands::Schema => {
            let mut out = std::io::stdout().lock();
            let mut err = std::io::stderr().lock();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style,
                fmt,
            };
            return schema::schema(&mut streams);
        }
        _ => {}
    }

    // Ahead of `resolve_paths`: `dev` computes its own `$SHEP_DEV_HOME`-rooted
    // paths, so the gate would refuse `shep dev` in a `$HOME`-less environment
    // for a reason the verb does not have. Unlocked handles: this runs until
    // the flock empties or a signal ends it.
    if let Commands::Dev(ref args) = cli.command {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style,
            fmt,
        };
        return dev::dev(
            &mut streams,
            cli.global.quiet,
            cli.global.home.is_some(),
            args,
        )
        .await;
    }

    let (paths, home_is_new) = match ensure_home(&cli.global) {
        Ok(resolved) => resolved,
        Err(refusal) => return report_home_refusal(fmt, &refusal),
    };
    if home_is_new {
        // Unconditional, unlike the welcome banner below: `shep welcome`
        // creates a fresh home too, and the scaffold that makes `shep start
        // server.js` work is owed there as well.
        scaffold_first_run_interpreters(&paths);
    }
    // `Welcome` is excluded: its own arm prints the same text to stdout a
    // moment later, and it should print once.
    if home_is_new && !matches!(cli.command, Commands::Welcome) {
        let mut err = std::io::stderr();
        let mut sink = std::io::sink();
        let mut streams = Streams {
            out: &mut sink,
            err: &mut err,
            style,
            fmt,
        };
        welcome::on_first_run(&mut streams, &paths.home, std::io::stderr().is_terminal());
    }

    // `dog` is a re-exec target like `daemon`, long-lived until signalled,
    // and writes straight to stderr rather than through a `Streams` envelope.
    if let Commands::Dog(ref args) = cli.command {
        return dog::run_dog(&args.name, paths).await;
    }

    // Split out of the dispatch below only to keep its handles unlocked; the
    // `unreachable!` at the bottom of that dispatch keeps the two in step.
    if let Commands::Bleats(ref args) = cli.command {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style,
            fmt,
        };
        return match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => bleats::bleats(&client, &mut streams, cli.global.quiet, args).await,
            Err(code) => code,
        };
    }

    // Not in the locked block below: this runs until the operator quits, and
    // owns stdout directly through the terminal.
    if let Commands::Lookout(ref args) = cli.command {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style,
            fmt,
        };
        return lookout::lookout(&mut streams, &paths, args, resolved_style).await;
    }

    // Not in the locked block below: `--foreground` runs until signalled, and
    // the quick registering half shares this function so the two flags cannot
    // validate differently.
    if let Commands::Serve(ref args) = cli.command {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style,
            fmt,
        };
        return serve_command(&mut streams, &paths, args).await;
    }

    // Not in the locked block below: this runs until the flock empties, and
    // the supervisor's logging boots in this same process, so an off-thread
    // write would be the first thing a guard wedged.
    if let Commands::Runtime(ref args) = cli.command {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style,
            fmt,
        };
        return runtime::runtime(&mut streams, cli.global.quiet, paths, args).await;
    }

    // No `Streams` at all: this verb owns stdout as a wire, everything written
    // there is MCP, and an `output::emit` would corrupt the peer's parse.
    if let Commands::Whistle = cli.command {
        let mut err = std::io::stderr();
        return whistle::whistle(&mut err, fmt, &paths).await;
    }

    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    let mut streams = Streams {
        out: &mut out,
        err: &mut err,
        style,
        fmt,
    };

    match cli.command {
        // No client: the welcome is local text about a local directory, and
        // asking a shepherd would fail on the fresh machine it greets.
        Commands::Welcome => {
            let shepherd = status::ShepherdStatus::probe(&paths).await;
            let code = welcome::welcome(&mut streams, &paths.home);
            // After the text: the status is the one line that changes between
            // runs.
            if fmt == Format::Table && std::io::stderr().is_terminal() {
                let _ = writeln!(streams.err, "{}", status::one_line(&shepherd));
            }
            code
        }
        Commands::Style(args) => match args.level {
            // Re-resolves rather than reading the `style` parameter, which
            // the hard rule may have forced to `Bare`: this report's job is
            // saying what is configured.
            None => {
                let (level, source) = resolve_style(&cli.global);
                let message = format!("{level} (from {source})");
                streams.note("style", &message);
                ExitCode::Success
            }
            // A level turns this from a report into a write. Config first,
            // report second, so a report cannot claim a write that failed.
            Some(level) => {
                // `try_edit`, not `edit`: `set_style_level` can refuse, and
                // `edit` saves after its closure whatever it returned, which
                // would rewrite a file this call reports as untouched.
                // `result_large_err` as in `commands::shep_toml`.
                #[cfg_attr(windows, allow(clippy::result_large_err))]
                if let Err(err) =
                    ShepToml::try_edit(&paths.daemon_config, |cfg| cfg.set_style_level(level))
                {
                    let code = match err {
                        ShepTomlError::Io { .. } => ExitCode::Failure,
                        ShepTomlError::Parse { .. } | ShepTomlError::WrongShape { .. } => {
                            ExitCode::InvalidConfig
                        }
                    };
                    return streams.fail(code, &err.to_string());
                }
                // Re-resolves, so the operator is told whether the value just
                // written will run or whether `--style`/`$SHEP_STYLE` still
                // outranks the `shep.toml` this call edited.
                let (effective, source) = resolve_style(&cli.global);
                let path = paths.daemon_config.display();
                let message = if style_write_is_overridden(source) {
                    format!(
                        "wrote {level} to {path}, but {source} still governs; \
                         shep runs at {effective}"
                    )
                } else {
                    format!("wrote {level} to {path}")
                };
                streams.note("style", &message);
                ExitCode::Success
            }
        },
        Commands::Start(ref args) => {
            load_command(&mut streams, &paths, guard, args, Load::Start).await
        }
        Commands::Add(ref args) => load_command(&mut streams, &paths, guard, args, Load::Add).await,
        Commands::Stop(ref args) | Commands::Thatlldo(ref args) => {
            match connect_client(&mut streams, &paths, guard).await {
                Ok(client) => lifecycle::stop(&client, &mut streams, args).await,
                Err(code) => code,
            }
        }
        Commands::Restart(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => lifecycle::restart(&client, &mut streams, &paths, args).await,
            Err(code) => code,
        },
        Commands::Reload(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => lifecycle::reload(&client, &mut streams, args).await,
            Err(code) => code,
        },
        Commands::Delete(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => lifecycle::delete(&client, &mut streams, args).await,
            Err(code) => code,
        },
        Commands::Stock(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => lifecycle::stock(&client, &mut streams, args).await,
            Err(code) => code,
        },
        Commands::Trigger(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => trigger::trigger(&client, &mut streams, args).await,
            Err(code) => code,
        },
        Commands::Signal(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => signal::signal(&client, &mut streams, args).await,
            Err(code) => code,
        },
        Commands::Whisper(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => whisper::whisper(&client, &mut streams, args).await,
            Err(code) => code,
        },
        // Falls back to the muster roll rather than refusing: looking at the
        // flock must not be a dead end on a machine that just rebooted.
        Commands::Flock(ref args) => flock_command(&mut streams, &paths, guard, args).await,
        // The guard arm is what makes `--available` work with no shepherd
        // running: it never reaches `connect_client`.
        Commands::Dogs(ref args) if args.available => {
            query::available_dogs(&mut streams, args).await
        }
        Commands::Dogs(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => query::dogs(&client, &mut streams, args).await,
            Err(code) => code,
        },
        // None of the four goes through the connect helpers: all four must
        // write the config with no shepherd running, so each connects
        // internally. `--exec` is the hidden pm2 spelling of `adopt`, mapped
        // here so `commands::dogs::enable` keeps its `&str` signature.
        Commands::Enable(ref args) => match &args.exec {
            Some(path) => {
                dogs::adopt(
                    &mut streams,
                    &paths,
                    &AdoptArgs {
                        path: path.clone(),
                        name: Some(args.name.clone()),
                    },
                )
                .await
            }
            None => dogs::enable(&mut streams, &paths, &args.name).await,
        },
        Commands::Disable(ref args) => dogs::disable(&mut streams, &paths, &args.name).await,
        Commands::Adopt(ref args) => dogs::adopt(&mut streams, &paths, args).await,
        Commands::Rehome(ref args) => dogs::rehome(&mut streams, &paths, &args.name).await,
        Commands::Describe(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => query::describe(&client, &mut streams, &paths, args).await,
            Err(code) => code,
        },
        Commands::Fold(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => query::fold(&client, &mut streams, &paths, args).await,
            Err(code) => code,
        },
        // Not `connect_client`: a verb reporting whether a shepherd answers
        // must not fail because the answer is "no". The exit code is still
        // `DaemonUnreachable`, so `shep ping && echo up` works.
        Commands::Ping => {
            let status = status::ShepherdStatus::probe(&paths).await;
            status::render_ping(&mut streams, &status)
        }
        // `connect_client`, not `connect_or_spawn_client`: autostarting a
        // daemon to save its empty flock would overwrite a good roll.
        Commands::Save => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => muster::save(&client, &mut streams).await,
            Err(code) => code,
        },
        Commands::Muster => match connect_or_spawn_client(&mut streams, &paths, guard).await {
            Ok(client) => muster::muster(&client, &mut streams).await,
            Err(code) => code,
        },
        Commands::Reopen(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => logs::reopen(&client, &mut streams, args).await,
            Err(code) => code,
        },
        // `--daemon` empties files this binary created and the daemon merely
        // inherited, so there is nothing to ask the socket. Not connecting is
        // the feature: a wedged shepherd is when this gets reached for.
        Commands::Flush(ref args) if args.daemon => logs::flush_daemon(&mut streams, &paths),
        Commands::Flush(ref args) => match connect_client(&mut streams, &paths, guard).await {
            Ok(client) => logs::flush(&client, &mut streams, args).await,
            Err(code) => code,
        },
        // Reads a file and starts nothing, so there is nothing to ask the
        // socket. The history is on disk so it survives the shepherd.
        Commands::Barks(ref args) => dogs::barks(&mut streams, &paths, args),
        // Reads and writes `kv.json` directly and never connects to the
        // shepherd.
        Commands::Set(ref args) => kv::set(&mut streams, &paths, args),
        Commands::Get(ref args) => kv::get(&mut streams, &paths, args),
        Commands::Unset(ref args) => kv::unset(&mut streams, &paths, args),
        // Same rule, same file-first reason: `shep secret set` before a
        // first `shep start` is the ordinary first-run order.
        Commands::Secret(ref args) => secret::secret(&mut streams, &paths, args),
        // Does its own connecting: `connect_client` reports and gives up,
        // which would leave an operator with a live daemon nothing can stop.
        Commands::Kill => admin::kill(&paths, &mut streams).await,
        Commands::Init(ref args) => init::init(&mut streams, args).await,
        // pm2 reads a file and writes a file; `env` writes an operator
        // override, which is the daemon's own store.
        Commands::Import(ref args) => match &args.command {
            ImportCommand::Pm2(args) => import::pm2::import(&mut streams, args),
            ImportCommand::Env(args) => match connect_client(&mut streams, &paths, guard).await {
                Ok(client) => import::dotenv::import_env(&client, &mut streams, &paths, args).await,
                Err(code) => code,
            },
        },
        Commands::Completions(_)
        | Commands::Daemon(_)
        | Commands::Startup(_)
        | Commands::Unstartup(_)
        | Commands::Schema
        | Commands::Bleats(_)
        | Commands::Lookout(_)
        | Commands::Whistle
        | Commands::Serve(_)
        | Commands::Runtime(_)
        | Commands::Dev(_)
        | Commands::Dog(_) => {
            unreachable!("handled above: before the shared $SHEP_HOME gate, or on unlocked handles")
        }
    }
}

/// What `shep startup`/`shep unstartup` say on Windows.
///
/// The rest of shep works here, so the message names the boundary: boot-time
/// supervision on Windows means a Service Control Manager service, a
/// different program shape from a unit template.
#[cfg(windows)]
const WINDOWS_NO_SERVICE: &str = "\
shep startup installs a boot-time service, and on Windows that means \
registering with the Service Control Manager -- not yet built (Tier B in \
docs/specs/windows-estimate.md).\n\
the shepherd itself works here: run `shep start` in your own session, or wrap \
`shep runtime` in a service manager such as NSSM or WinSW.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::style_from_config;

    use shep_core::paths::ShepPaths;
    /// The two verbs are told apart by `--home`: `startup` refuses a
    /// `$SHEP_HOME` that is not there, and `unstartup` ignores `--home`, since
    /// a removal is addressed by the unit's path and label.
    ///
    /// Skipped as root: `unstartup` would reach a real `systemctl` or
    /// `launchctl` against whatever this machine has installed.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn startup_and_unstartup_reach_their_own_verbs() {
        use clap::Parser;

        if nix::unistd::geteuid().is_root() {
            eprintln!("skipping: as root these verbs really install and remove a system unit");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let missing = missing.to_str().unwrap();

        let cli = Cli::try_parse_from(["shep", "--home", missing, "startup"]).unwrap();
        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Usage,
            "startup must refuse a $SHEP_HOME that is not there"
        );

        let cli = Cli::try_parse_from(["shep", "--home", missing, "unstartup"]).unwrap();
        assert_ne!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Usage,
            "unstartup removes a unit and never reads the home a --home names"
        );
    }

    /// What `shep style <level>` writes must be what `style_from_config`, the
    /// reader `resolve_style` uses, reads back.
    ///
    /// `#[cfg(unix)]`: the write goes through `commands::shep_toml`'s
    /// `ConfigLock`, whose `cfg(windows)` arm nothing in this crate executes.
    #[cfg(unix)]
    #[tokio::test]
    async fn style_with_a_level_writes_shep_toml_and_the_config_reads_it_back() {
        use clap::Parser;

        for (raw, expected) in [
            ("full", style::StyleLevel::Full),
            ("plain", style::StyleLevel::Plain),
            ("bare", style::StyleLevel::Bare),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().to_str().unwrap();
            let cli = Cli::try_parse_from(["shep", "--home", home, "style", raw]).unwrap();
            assert_eq!(
                run(
                    cli,
                    style::Presentation::BARE,
                    (style::StyleLevel::Full, style::StyleSource::Default),
                )
                .await,
                ExitCode::Success,
                "style {raw}"
            );

            let written = std::fs::read_to_string(dir.path().join("shep.toml")).unwrap();
            assert_eq!(
                style_from_config(Some(&written)),
                Some(expected),
                "style {raw}"
            );
        }
    }

    /// The no-arg form is a report and only a report. `#[cfg(unix)]` to stay
    /// paired with the test above.
    #[cfg(unix)]
    #[tokio::test]
    async fn style_with_no_level_reports_and_writes_nothing() {
        use clap::Parser;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let cli = Cli::try_parse_from(["shep", "--home", home, "style"]).unwrap();
        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Success
        );

        assert!(
            !dir.path().join("shep.toml").exists(),
            "the no-arg form must not create a shep.toml that was not there"
        );
    }

    /// Pins the structural rule: `completions` never reaches `resolve_paths`.
    ///
    /// It cannot pin the behaviour. With `$HOME` set, a reinstated
    /// `resolve_paths` would succeed and fall through to the same call, so
    /// `run` returns `Success` either way, and unsetting `$HOME` here is
    /// `unsafe` in edition 2024. The e2e tier spawns the real binary with
    /// `$HOME` cleared instead.
    #[tokio::test]
    async fn completions_never_resolves_paths() {
        use clap::Parser;
        let argv = ["shep", "completions", "bash"];
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed: {e}"));
        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::Success
        );
    }

    /// Drives `kill`'s dispatch arm, the one with its own connect, against a
    /// home no shepherd owns.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_dispatches_kill_to_the_socket_free_path() {
        use clap::Parser;

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        // `pids/` holds the lock `kill`'s socket-free path reads, `run/` the
        // control socket it tries first.
        let paths = ShepPaths::resolve(
            &|key| (key == "SHEP_HOME").then(|| home.to_string_lossy().into_owned()),
            std::path::Path::new("/nonexistent"),
        );
        std::fs::create_dir_all(&paths.pids).unwrap();
        std::fs::create_dir_all(&paths.run).unwrap();
        let argv = ["shep", "--home", home.to_str().unwrap(), "kill"];
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{argv:?} failed: {e}"));

        assert_eq!(
            run(
                cli,
                style::Presentation::BARE,
                (style::StyleLevel::Full, style::StyleSource::Default),
            )
            .await,
            ExitCode::DaemonUnreachable,
            "`kill` against an unowned home must reach its own socket-free path"
        );
    }
}
