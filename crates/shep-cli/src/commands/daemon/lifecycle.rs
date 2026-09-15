//! Boot and lifecycle: config load, the log subscriber, [`boot_supervisor`]
//! and [`run_daemon`], and the exit-code mapping for both.

use std::ffi::OsStr;
use std::io::IsTerminal;

use shep_core::config::{DaemonConfig, DaemonConfigError, DaemonOverrides};
use shep_core::paths::ShepPaths;
use shep_core::protocol::DogSource;
use shep_core::values::UpDuration;
use shep_daemon::boot::{BootError, BootOptions, RunningDaemon, boot};
use shep_daemon::dogs::DogSpec;
#[cfg(unix)]
use shep_daemon::notify::NOTIFY_SOCKET_ENV;
use shep_daemon::tokio_runner::TokioRunner;
use tracing_subscriber::EnvFilter;

use crate::cli::DaemonArgs;
use crate::commands::dog_migration::{self, DogMigrationError};
use crate::exit::ExitCode;

/// Everything [`run_daemon`] can fail with.
///
/// [`Self::Boot`] and [`Self::Run`] both wrap a [`BootError`] and stay
/// distinct: `Run` means the supervisor came up and served.
#[derive(Debug)]
pub enum DaemonRunError {
    /// `shep.toml` was unreadable as config.
    Config(DaemonConfigError),
    /// The supervisor failed to come up, before it ever served a request.
    Boot(BootError),
    /// The supervisor came up and served, then failed during its run loop
    /// or teardown.
    Run(BootError),
    /// `[dog.<name>]` sections could not be moved into `dogs.toml`. Raised
    /// before the supervisor comes up, so no dog has read a section from
    /// either file yet.
    DogMigration(DogMigrationError),
}

impl core::fmt::Display for DaemonRunError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Config(err) => write!(f, "invalid daemon configuration: {err}"),
            Self::Boot(err) => write!(f, "the daemon failed to boot: {err}"),
            Self::Run(err) => write!(f, "the daemon failed while running: {err}"),
            Self::DogMigration(err) => write!(f, "invalid dog configuration: {err}"),
        }
    }
}

impl core::error::Error for DaemonRunError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Config(err) => Some(err),
            Self::Boot(err) | Self::Run(err) => Some(err),
            Self::DogMigration(err) => Some(err),
        }
    }
}

impl From<DaemonConfigError> for DaemonRunError {
    fn from(source: DaemonConfigError) -> Self {
        Self::Config(source)
    }
}

// No `impl From<BootError>`: both `Boot` and `Run` wrap one, so each call
// site picks with an explicit `map_err`.

/// Loads `paths.daemon_config`'s raw source, `None` for a missing file.
///
/// Only `NotFound` is swallowed: any other IO failure is a fault on a real
/// path, and is reported through [`DaemonRunError::Boot`].
pub(super) fn read_daemon_config_source(
    paths: &ShepPaths,
) -> Result<Option<String>, DaemonRunError> {
    match std::fs::read_to_string(&paths.daemon_config) {
        Ok(src) => Ok(Some(src)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(DaemonRunError::Boot(BootError::Io {
            path: paths.daemon_config.clone(),
            source,
        })),
    }
}

/// Installs the subscriber that renders the daemon's own records, for the
/// remaining life of this process.
///
/// The one global install in the workspace; `shep-daemon`'s
/// `testing::capture_logs` installs a scoped one per test. The sink is
/// stderr, which `launch.rs` has already redirected into
/// `$SHEP_HOME/logs/shepd.err.log` for a re-exec'd daemon.
///
/// Records are written from tokio worker threads, so `main::run`'s `daemon`
/// arm must not hold a `stderr().lock()` guard while this process runs. A
/// failed install means a subscriber is already there, and is reported on
/// stderr rather than failing the boot.
fn install_log_subscriber(config: &DaemonConfig) {
    let builder = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(config.daemon.log_level.as_str()))
        .with_writer(std::io::stderr);
    let installed = if config.daemon.log_json {
        builder.json().try_init()
    } else {
        builder
            .with_ansi(ansi_enabled(
                std::io::stderr().is_terminal(),
                std::env::var_os("NO_COLOR").as_deref(),
            ))
            .try_init()
    };
    if let Err(err) = installed {
        eprintln!("shep: the daemon's own logs are not being rendered: {err}");
    }
}

/// Whether ANSI colour belongs on the daemon's own records: only when stderr
/// is a terminal, and only when `NO_COLOR` is unset or empty.
///
/// `RUST_LOG` is ignored; `[daemon] log_level` and `SHEP_LOG_LEVEL` are the
/// only level knobs. An empty `NO_COLOR` is an unset one, per the convention.
fn ansi_enabled(stderr_is_terminal: bool, no_color: Option<&OsStr>) -> bool {
    stderr_is_terminal && no_color.is_none_or(OsStr::is_empty)
}

/// Loads config, installs the log subscriber, and boots the supervisor:
/// everything [`run_daemon`] does except serve.
///
/// Separate from [`run_daemon`] so `commands::foreground` can hold the booted
/// daemon rather than block until shutdown. The log subscriber is global, so
/// it is installed here rather than in `shep_daemon::boot`, which one test
/// binary calls many times.
///
/// # Errors
/// - [`DaemonRunError::Config`]: `shep.toml` or a `SHEP_*` override is invalid.
/// - [`DaemonRunError::Boot`]: the config file was unreadable, or the boot failed.
/// - [`DaemonRunError::DogMigration`]: `[dog]` sections could not be moved.
pub async fn boot_supervisor(
    paths: ShepPaths,
    args: &DaemonArgs,
    delete_flock_on_shutdown: bool,
) -> Result<RunningDaemon, DaemonRunError> {
    // Before the config load and before the supervisor: `dog_section` reads
    // the new file from the first request onward, and a dog can connect as
    // soon as the socket is up. Idempotent, and takes both files' locks in
    // this crate's one order, shep.toml outer, dogs.toml inner.
    let moved =
        dog_migration::migrate_dog_sections(&paths).map_err(DaemonRunError::DogMigration)?;
    if !moved.is_empty() {
        // Not `tracing`: `install_log_subscriber` runs below, so a record
        // written here would be dropped.
        eprintln!(
            "shep: moved dog config out of shep.toml and into dogs.toml: {}",
            moved.join(", ")
        );
    }
    let env = |key: &str| std::env::var(key).ok();
    let file_source = read_daemon_config_source(&paths)?;
    let overrides = daemon_overrides(args);
    let config = DaemonConfig::load_layered(file_source.as_deref(), &env, &overrides)?;
    install_log_subscriber(&config);
    // The one read of `$NOTIFY_SOCKET` in the workspace. Unix only: it is
    // systemd's readiness protocol, and the field stays on `BootOptions` for
    // both platforms.
    #[cfg(unix)]
    let notify_socket = std::env::var_os(NOTIFY_SOCKET_ENV);
    #[cfg(windows)]
    let notify_socket: Option<std::ffi::OsString> = None;
    let mut options = boot_options(&config, args, notify_socket.as_deref());
    options.delete_flock_on_shutdown = delete_flock_on_shutdown;
    let daemon = boot(TokioRunner::new(), paths, options)
        .await
        .map_err(DaemonRunError::Boot)?;
    // The one publisher of `config.dog.<name>`: the migration above ran
    // before `boot`, when there was no bus to publish onto.
    daemon.context().announce_dog_config(&moved);
    Ok(daemon)
}

/// Runs the supervisor in this process until a signal or `KillDaemon`.
///
/// [`boot_supervisor`] plus `.run()`.
///
/// # Errors
/// - [`DaemonRunError::Config`]: `shep.toml` or a `SHEP_*` override is invalid.
/// - [`DaemonRunError::Boot`]: the config file was unreadable, or the boot failed.
/// - [`DaemonRunError::Run`]: the supervisor served, then failed in its run
///   loop or teardown.
/// - [`DaemonRunError::DogMigration`]: `[dog]` sections could not be moved.
pub async fn run_daemon(paths: ShepPaths, args: &DaemonArgs) -> Result<(), DaemonRunError> {
    // A production daemon always keeps its final roll: `shep muster` after a
    // reboot reads it.
    boot_supervisor(paths, args, false)
        .await?
        .run()
        .await
        .map_err(DaemonRunError::Run)
}

/// Builds the CLI-flag layer of `file < env < flags` from the `daemon`
/// subcommand's own arguments.
#[must_use]
pub fn daemon_overrides(args: &DaemonArgs) -> DaemonOverrides {
    DaemonOverrides::new()
        .log_json(args.log_json)
        .log_level(args.log_level)
        .socket(args.socket.clone())
        .max_cron_sleep(args.max_cron_sleep)
}

/// Builds [`BootOptions`] from `config`, the `daemon` subcommand's own
/// flags, and whatever `$NOTIFY_SOCKET` held.
///
/// `ready_fd` stays `None`: readiness is a completed handshake, and this crate
/// forbids unsafe code. `max_cron_sleep` stays an `Option`, so the daemon
/// applies its own default and nothing here invents a value. `notify_socket`
/// is a parameter rather than an environment read, since `std::env::set_var`
/// is `unsafe` in edition 2024; `--foreground` gates it.
///
/// `[daemon] enabled_dogs` names each dog to start, in the order an operator
/// wrote it; `[daemon] adopted_dogs` says which of those names is a
/// third-party binary, and a name absent from it is [`DogSource::BuiltIn`].
#[must_use]
pub fn boot_options(
    config: &DaemonConfig,
    args: &DaemonArgs,
    notify_socket: Option<&OsStr>,
) -> BootOptions {
    BootOptions {
        socket: config.daemon.socket.clone(),
        ready_fd: None,
        restore: !args.no_restore,
        environment: Some(config.daemon.environment.clone()),
        max_cron_sleep: config.daemon.max_cron_sleep.map(UpDuration::as_duration),
        notify_socket: notify_socket
            .filter(|_| args.foreground)
            .map(OsStr::to_os_string),
        dogs: config
            .daemon
            .enabled_dogs
            .iter()
            .map(|name| {
                let source = match config.daemon.adopted_dogs.get(name) {
                    Some(path) => DogSource::Adopted {
                        path: path.display().to_string(),
                    },
                    None => DogSource::BuiltIn,
                };
                DogSpec {
                    name: name.clone(),
                    source,
                }
            })
            .collect(),
        // Every dog that EXISTS, which is not the list above: that one is
        // the spawn order and holds only what an operator switched on.
        // `Request::SetDogConfig` is guarded on this one, because the dog
        // most in need of configuring is the one that is disabled or has
        // never started. The same two sources `fail_enable_unknown_dog`
        // calls valid names, plus `enabled_dogs` itself, so a name a
        // hand-edited `shep.toml` enables without adopting is still a dog
        // this shepherd tries to spawn and still one it may hold a section
        // for.
        known_dogs: crate::dog::BUILT_IN_DOGS
            .iter()
            .map(|built_in| (*built_in).to_string())
            .chain(config.daemon.adopted_dogs.keys().cloned())
            .chain(config.daemon.enabled_dogs.iter().cloned())
            .collect(),
        boot_first_dogs: config.daemon.boot_first_dogs.clone(),
        // Overwritten by `boot_supervisor`, the only caller that ever wants
        // `true`.
        delete_flock_on_shutdown: false,
        // This process is the shep binary, which is what the field asserts, so
        // an init system's SIGHUP reaches the handover `shep daemon reload` does.
        handover: true,
    }
}

/// Maps a boot or run failure to the process exit status the parent will read.
///
/// [`BootError`] is `#[non_exhaustive]`, so the [`DaemonRunError::Boot`] arm
/// carries a wildcard and only [`BootError::AlreadyRunning`] gets its own
/// code. [`DaemonRunError::Run`] is always [`ExitCode::Failure`], since
/// `RunningDaemon::run()` names only `BootError::Io`.
#[must_use]
pub fn daemon_exit_code(err: &DaemonRunError) -> ExitCode {
    match err {
        DaemonRunError::Config(_) => ExitCode::InvalidConfig,
        DaemonRunError::Boot(boot_err) => match boot_err {
            BootError::AlreadyRunning { .. } => ExitCode::DaemonAlreadyRunning,
            // `Io`/`Snapshot`/`ReadyWrite` today, plus any future variant
            // `#[non_exhaustive]` makes room for.
            _ => ExitCode::Failure,
        },
        DaemonRunError::Run(_) => ExitCode::Failure,
        // Every refusal this variant carries, I/O faults included, is a
        // shep.toml or dogs.toml an operator has to edit.
        DaemonRunError::DogMigration(_) => ExitCode::InvalidConfig,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shep_core::config::LogLevel;

    #[test]
    fn every_daemon_flag_reaches_the_config() {
        let args = DaemonArgs {
            cmd: None,
            no_restore: false,
            foreground: false,
            log_json: Some(true),
            log_level: Some(LogLevel::Trace),
            socket: Some(std::path::PathBuf::from("/tmp/flag.sock")),
            max_cron_sleep: Some(UpDuration::from_millis(120_000)),
        };
        let cfg = DaemonConfig::load_layered(
            Some(
                "[daemon]\nlog_json = false\nlog_level = \"error\"\nsocket = \"/tmp/file.sock\"\n",
            ),
            &|_| None,
            &daemon_overrides(&args),
        )
        .unwrap();
        assert!(cfg.daemon.log_json);
        assert_eq!(cfg.daemon.log_level, LogLevel::Trace);
        assert_eq!(
            cfg.daemon.socket,
            Some(std::path::PathBuf::from("/tmp/flag.sock"))
        );
        assert_eq!(
            cfg.daemon.max_cron_sleep,
            Some(UpDuration::from_millis(120_000))
        );
    }

    #[test]
    fn colour_needs_a_terminal_and_no_no_color() {
        assert!(ansi_enabled(true, None));
        assert!(!ansi_enabled(false, None), "a file never gets escape codes");
        assert!(!ansi_enabled(true, Some(OsStr::new("1"))));
        assert!(
            ansi_enabled(true, Some(OsStr::new(""))),
            "an empty NO_COLOR is an unset NO_COLOR"
        );
        assert!(
            !ansi_enabled(false, Some(OsStr::new("1"))),
            "the two reasons to suppress colour must not cancel out"
        );
    }

    #[test]
    fn boot_options_pass_ready_fd_none_and_the_configured_socket() {
        let config =
            DaemonConfig::load(Some("[daemon]\nsocket = \"/tmp/custom.sock\"\n"), &|_| None)
                .unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert!(
            opts.ready_fd.is_none(),
            "readiness is a handshake in this phase"
        );
        assert_eq!(
            opts.socket.as_deref(),
            Some(std::path::Path::new("/tmp/custom.sock"))
        );
        assert!(opts.restore, "the default is to restore the muster roll");
    }

    #[test]
    fn boot_options_carry_every_enabled_dog_with_the_source_the_file_names() {
        let src = r#"
[daemon]
enabled_dogs = ["metrics", "otel"]

[daemon.adopted_dogs]
otel = "/usr/local/bin/shep-otel"
"#;
        let config = DaemonConfig::load(Some(src), &|_| None).unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert_eq!(
            opts.dogs,
            vec![
                DogSpec {
                    name: "metrics".into(),
                    source: DogSource::BuiltIn
                },
                DogSpec {
                    name: "otel".into(),
                    source: DogSource::Adopted {
                        path: "/usr/local/bin/shep-otel".into()
                    }
                },
            ]
        );
    }

    // fails if the key parses but never reaches the daemon, which would
    // leave log-rotate starting after the flock it exists to serve
    #[test]
    fn boot_options_carries_the_promoted_dogs() {
        let src = r#"
[daemon]
enabled_dogs = ["metrics", "log-rotate"]
boot_first_dogs = ["log-rotate"]
"#;
        let config = DaemonConfig::load(Some(src), &|_| None).unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert_eq!(opts.boot_first_dogs, vec!["log-rotate".to_string()]);
    }

    #[test]
    fn boot_options_know_every_dog_that_exists_and_not_only_the_enabled_ones() {
        let src = r#"
[daemon]
enabled_dogs = ["metrics"]

[daemon.adopted_dogs]
otel = "/usr/local/bin/shep-otel"
"#;
        let config = DaemonConfig::load(Some(src), &|_| None).unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );

        let known: std::collections::BTreeSet<&str> =
            opts.known_dogs.iter().map(String::as_str).collect();
        assert_eq!(
            known,
            ["bark", "metrics", "otel"].into_iter().collect(),
            "adopted-and-disabled is the case this field exists for"
        );
    }

    #[test]
    fn boot_options_carry_the_configured_max_cron_sleep_and_invent_none() {
        let configured =
            DaemonConfig::load(Some("[daemon]\nmax_cron_sleep = \"5m\"\n"), &|_| None).unwrap();
        assert_eq!(
            boot_options(
                &configured,
                &DaemonArgs {
                    cmd: None,
                    no_restore: false,
                    foreground: false,
                    log_json: None,
                    log_level: None,
                    socket: None,
                    max_cron_sleep: None,
                },
                None
            )
            .max_cron_sleep,
            Some(core::time::Duration::from_secs(300))
        );

        let unset = DaemonConfig::load(None, &|_| None).unwrap();
        assert_eq!(
            boot_options(
                &unset,
                &DaemonArgs {
                    cmd: None,
                    no_restore: false,
                    foreground: false,
                    log_json: None,
                    log_level: None,
                    socket: None,
                    max_cron_sleep: None,
                },
                None
            )
            .max_cron_sleep,
            None,
            "an unset knob must stay None: the daemon owns the default"
        );
    }

    #[test]
    fn boot_options_carry_the_configured_environment_and_default_to_production() {
        // The host default every sheep naming no `environment` of its own
        // resolves its `{{secret:...}}` references in, so an unset file
        // reaching the supervisor as anything but `production` would move
        // every such sheep to a different slot of the store.
        let args = || DaemonArgs {
            cmd: None,
            no_restore: false,
            foreground: false,
            log_json: None,
            log_level: None,
            socket: None,
            max_cron_sleep: None,
        };

        let configured =
            DaemonConfig::load(Some("[daemon]\nenvironment = \"staging\"\n"), &|_| None).unwrap();
        assert_eq!(
            boot_options(&configured, &args(), None).environment,
            Some("staging".to_string())
        );

        let unset = DaemonConfig::load(None, &|_| None).unwrap();
        assert_eq!(
            boot_options(&unset, &args(), None).environment,
            Some("production".to_string())
        );
    }

    #[test]
    fn no_restore_boots_without_the_muster_roll() {
        let config = DaemonConfig::load(None, &|_| None).unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: true,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert!(!opts.restore);
    }

    #[test]
    fn the_foreground_flag_reaches_the_boot_options() {
        let config = DaemonConfig::load(None, &|_| None).unwrap();
        let bare = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert!(
            bare.notify_socket.is_none(),
            "an autostarted daemon reports to nobody"
        );

        let supervised = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: true,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            Some(OsStr::new("/run/systemd/notify")),
        );
        assert_eq!(
            supervised.notify_socket.as_deref(),
            Some(OsStr::new("/run/systemd/notify"))
        );

        // Without the flag the address is ignored, so a shep autostarted
        // inside another notify-type service cannot report readiness by accident.
        let unflagged = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: true,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            None,
        );
        assert!(unflagged.notify_socket.is_none());

        let inherited = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: false,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            Some(OsStr::new("/run/systemd/notify")),
        );
        assert!(inherited.notify_socket.is_none());
    }

    #[test]
    fn foreground_and_no_restore_are_independent() {
        let config = DaemonConfig::load(None, &|_| None).unwrap();
        let opts = boot_options(
            &config,
            &DaemonArgs {
                cmd: None,
                no_restore: false,
                foreground: true,
                log_json: None,
                log_level: None,
                socket: None,
                max_cron_sleep: None,
            },
            Some(OsStr::new("/run/systemd/notify")),
        );
        assert!(opts.restore, "a supervised daemon still musters its roll");
    }

    #[test]
    fn already_running_gets_its_own_exit_code_and_everything_else_is_failure() {
        use DaemonRunError::{Boot, Config, Run};
        assert_eq!(
            daemon_exit_code(&Boot(BootError::AlreadyRunning { pid: Some(7) })),
            ExitCode::DaemonAlreadyRunning
        );
        assert_eq!(
            daemon_exit_code(&Boot(BootError::AlreadyRunning { pid: None })),
            ExitCode::DaemonAlreadyRunning
        );
        assert_eq!(
            daemon_exit_code(&Boot(BootError::Io {
                path: "/x".into(),
                source: std::io::Error::other("x"),
            })),
            ExitCode::Failure
        );
        assert_eq!(
            daemon_exit_code(&Run(BootError::Io {
                path: "/x".into(),
                source: std::io::Error::other("x"),
            })),
            ExitCode::Failure
        );
        assert_eq!(
            daemon_exit_code(&Config(DaemonConfigError::Toml("expected `=`".into()))),
            ExitCode::InvalidConfig
        );
        assert_eq!(
            daemon_exit_code(&Config(DaemonConfigError::BadEnvValue(
                "SHEP_LOG_JSON",
                "maybe".into()
            ))),
            ExitCode::InvalidConfig
        );
        assert_eq!(
            daemon_exit_code(&DaemonRunError::DogMigration(
                DogMigrationError::WouldOverwrite {
                    name: "metrics".to_string(),
                }
            )),
            ExitCode::InvalidConfig
        );
    }

    #[test]
    fn boot_and_run_report_different_phases_for_the_same_underlying_error() {
        use DaemonRunError::{Boot, Run};
        let io_err = || BootError::Io {
            path: "/x".into(),
            source: std::io::Error::other("x"),
        };
        let boot_msg = Boot(io_err()).to_string();
        let run_msg = Run(io_err()).to_string();
        assert_ne!(boot_msg, run_msg);
        assert!(boot_msg.starts_with("the daemon failed to boot"));
        assert!(
            !run_msg.starts_with("the daemon failed to boot"),
            "a run-phase failure must not still claim to be a boot failure: {run_msg:?}"
        );
        assert!(run_msg.starts_with("the daemon failed while running"));
    }
}
