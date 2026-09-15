//! Daemon connectivity: connect-or-autostart, the `start`/`add` Flockfile
//! load path, the bare-shepherd boot, and the foreground daemon command.

use cli::{DaemonArgs, Format, GlobalArgs, StartArgs};
use commands::daemon::{daemon_exit_code, run_daemon};
use commands::lifecycle::{self, Load};
use exit::ExitCode;
use launch::launch_daemon;
use output::Streams;
use shep_client::Client;
use shep_client::spawn::{SpawnOutcome, connect_or_spawn};
use shep_core::paths::ShepPaths;

use crate::config::interpreters_from_config;
use crate::home::{report_home_refusal, resolve_paths};
use crate::version_guard::{VersionGuard, refuse_version_skew};
use crate::{cli, commands, exit, launch, output, status};

/// Emits one error envelope to stderr under a lock taken for just that write.
///
/// The lock keeps the envelope whole: under `--format json`
/// [`output::emit_error`] writes it as many small writes on an unbuffered
/// `Stderr`, and a record from a worker thread landing between two of them
/// tears the envelope in half.
pub(crate) fn emit_error_locked(fmt: Format, code: ExitCode, message: &str) {
    let mut err = std::io::stderr().lock();
    let _ = output::emit_error(&mut err, fmt, code.code_str(), message);
}

/// Connects to the daemon at `paths.socket`, autostarting one via
/// [`launch_daemon`] if nothing answers. `Start` and `Muster` are the two
/// arms that dispatch through this rather than [`crate::version_guard::connect_client`].
///
/// `pub(crate)`: `commands::daemon`'s reload starts the successor it just
/// stopped through this same autostart.
pub(crate) async fn connect_or_spawn_client(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: VersionGuard,
) -> Result<Client, ExitCode> {
    let launch_paths = paths.clone();
    match connect_or_spawn(&paths.socket, move || launch_daemon(&launch_paths)).await {
        // One arm for both outcomes: a daemon just spawned is this same binary
        // and cannot skew, and one already up is what the guard is for.
        Ok(SpawnOutcome::Connected(client) | SpawnOutcome::Spawned(client)) => {
            refuse_version_skew(streams, &client, guard)?;
            Ok(client)
        }
        Err(err) => {
            let code = ExitCode::from(&err);
            Err(streams.fail(code, &err.to_string()))
        }
    }
}

/// `shep start` and `shep add`, which share everything but one arm.
///
/// Bare, either verb means the Flockfile in this directory. The two disagree
/// only about what an empty directory means.
pub(crate) async fn load_command(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: VersionGuard,
    args: &StartArgs,
    mode: Load,
) -> ExitCode {
    let discovered = if args.targets.is_empty() {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| shep_core::config::flockfile::discover(&cwd))
    } else {
        None
    };
    if args.targets.is_empty() && discovered.is_none() {
        // Bare `shep start` in an empty directory means "bring a shepherd
        // up". `shep add` cannot mean that: it would register nothing.
        return match mode {
            Load::Start => start_bare_shepherd(streams, paths, guard).await,
            Load::Add => streams.fail(
                ExitCode::Usage,
                "no target and no Flockfile in this directory",
            ),
        };
    }
    let shep_toml_text = std::fs::read_to_string(&paths.daemon_config).ok();
    let interpreters = interpreters_from_config(shep_toml_text.as_deref());
    let client = match connect_or_spawn_client(streams, paths, guard).await {
        Ok(client) => client,
        Err(code) => return code,
    };
    match mode {
        Load::Start => {
            lifecycle::start(&client, streams, args, discovered.as_deref(), &interpreters).await
        }
        Load::Add => {
            lifecycle::add(&client, streams, args, discovered.as_deref(), &interpreters).await
        }
    }
}

/// `shep start` with no target and no Flockfile in sight: bring a shepherd up
/// and stop there.
///
/// The only way to get a shepherd without also starting a process: every
/// other route needs a target or a saved roll. Reports rather than re-boots
/// when one is already up.
pub(crate) async fn start_bare_shepherd(
    streams: &mut Streams<'_>,
    paths: &ShepPaths,
    guard: VersionGuard,
) -> ExitCode {
    let before = status::ShepherdStatus::probe(paths).await;
    if let Some(online) = &before.online {
        let message = format!(
            "shepherd already up (pid {}). `shep start <target>` adds a sheep.",
            online.pid
        );
        streams.aside("start", &message);
        return ExitCode::Success;
    }
    match connect_or_spawn_client(streams, paths, guard).await {
        Ok(client) => {
            // Asked after the boot: bringing the shepherd up restores the
            // muster roll, so a flock that looked empty may have members now.
            let restored = client
                .request(shep_core::protocol::Request::ListFlock)
                .await;
            let known = match &restored {
                Ok(shep_core::protocol::Response::Flock(procs)) => procs.len(),
                _ => 0,
            };
            let message = if known == 0 {
                format!(
                    "shepherd up, flock at {}. Nothing running yet; \
                     `shep start <target>` adds a sheep.",
                    paths.home.display()
                )
            } else {
                format!(
                    "shepherd up, flock at {}. {known} sheep restored from the roll; \
                     `shep flock` lists them.",
                    paths.home.display()
                )
            };
            streams.note("start", &message);
            ExitCode::Success
        }
        Err(code) => code,
    }
}

/// Renders a failed connect for an operator rather than for a library caller.
///
/// The absent-socket case gets its own sentence and the next command: on a
/// machine where no shepherd has ever run, `shep-client`'s `ENOENT` reads as
/// a broken install, about a path the operator did not choose. Every other
/// failure keeps the library's wording, since `EACCES` and `ECONNREFUSED`
/// each mean something specific.
///
/// `pub(crate)`: `version_guard::connect_client` reports a failed connect
/// through this same wording.
pub(crate) fn unreachable_message(err: &shep_client::ConnectError) -> String {
    match err {
        shep_client::ConnectError::Connect { path, source }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            format!(
                "no shepherd is running (no socket at `{}`); \
                 start one with `shep start <target>`",
                path.display()
            )
        }
        other => other.to_string(),
    }
}

/// Resolves this invocation's own [`ShepPaths`] and runs the supervisor in
/// the foreground until a signal or `KillDaemon`.
///
/// Takes no [`Streams`] of its own: the supervisor writes its diagnostics
/// through the subscriber `commands::daemon::install_log_subscriber`
/// installs, and the two error envelopes here each go under their own
/// short-lived lock.
pub(crate) async fn run_daemon_command(
    fmt: Format,
    global: &GlobalArgs,
    args: &DaemonArgs,
) -> ExitCode {
    let paths = match resolve_paths(global) {
        Ok(paths) => paths,
        Err(refusal) => return report_home_refusal(fmt, &refusal),
    };
    match run_daemon(paths, args).await {
        Ok(()) => ExitCode::Success,
        Err(err) => {
            let code = daemon_exit_code(&err);
            emit_error_locked(fmt, code, &err.to_string());
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ENOENT` is what a person meets where no shepherd has ever run.
    /// `EACCES` is a socket this user may not have, and `shep start` would be
    /// the wrong fix for it.
    #[cfg(unix)]
    #[test]
    fn an_absent_socket_names_the_next_command_and_other_failures_do_not() {
        use std::io::{Error, ErrorKind};

        let absent = shep_client::ConnectError::Connect {
            path: std::path::PathBuf::from("/root/.shep/run/shep.sock"),
            source: Error::from(ErrorKind::NotFound),
        };
        assert_eq!(
            unreachable_message(&absent),
            "no shepherd is running (no socket at `/root/.shep/run/shep.sock`); \
             start one with `shep start <target>`"
        );

        let denied = shep_client::ConnectError::Connect {
            path: std::path::PathBuf::from("/root/.shep/run/shep.sock"),
            source: Error::from(ErrorKind::PermissionDenied),
        };
        let text = unreachable_message(&denied);
        assert!(
            text.starts_with("could not connect to `/root/.shep/run/shep.sock`:"),
            "a permission failure must keep the library's wording, got {text:?}"
        );
        assert!(
            !text.contains("shep start"),
            "a permission failure must not send the operator to `shep start`, got {text:?}"
        );
    }
}
