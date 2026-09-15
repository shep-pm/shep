//! The control socket, and the readiness line written the moment it is bound
//!
//! [`bind_socket`] is the only place a crashed predecessor's leftover socket
//! is recovered from, and it is why the pidfile lock is taken first: the
//! probe-then-unlink it performs is safe only while one daemon at a time can
//! run it. [`write_ready`] answers the parent that re-exec'd this process,
//! which is waiting on the bind and nothing later.

#[cfg(unix)]
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shep_core::paths::ShepPaths;
use shep_core::transport::Listener;

use super::BootError;
use super::pidfile::read_pidfile;

/// The socket this daemon binds: the layout default, or a config override
#[must_use]
pub(crate) fn socket_path(paths: &ShepPaths, override_path: Option<&Path>) -> PathBuf {
    match override_path {
        Some(path) => path.to_path_buf(),
        None => paths.socket.clone(),
    }
}

/// Warns, and does not refuse, when `socket`'s directory is reachable by
/// anyone but its owner. [`init_dirs`](super::init_dirs) leaves the default layout's `run/` at
/// `0700`, so this fires only for a `[daemon].socket` override pointed
/// somewhere looser.
#[cfg(unix)]
fn warn_if_socket_dir_is_loose(socket: &Path) {
    let Some(parent) = socket.parent() else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(parent) else {
        return;
    };
    if metadata.permissions().mode() & 0o022 != 0 {
        tracing::warn!(
            path = %parent.display(),
            "control-socket directory is group- or world-writable; \
             the 0700 guarantee only covers the default $SHEP_HOME/run"
        );
    }
}

/// Binds the control socket, recovering from a crashed daemon's leftovers
///
/// # Errors
/// - [`BootError::AlreadyRunning`] if a live daemon answered on the socket.
/// - [`BootError::Io`] if bind, probe, or unlink failed.
// The Windows arm's `return` is load-bearing: the `cfg(unix)` block after it
// is the rest of the function, and the unix arm names types Windows lacks.
#[allow(clippy::needless_return)]
pub(crate) fn bind_socket(paths: &ShepPaths, socket: &Path) -> Result<Listener, BootError> {
    // A named pipe is not a file: no `sun_path` limit, no containing directory
    // mode, and nothing left on disk to probe when its owner dies. The kernel
    // enforces the exclusion instead, through `Listener::bind`'s
    // `first_pipe_instance`.
    #[cfg(windows)]
    {
        /// What `first_pipe_instance` reports when the pipe name already has
        /// an owner: another daemon rather than a genuine I/O failure.
        const ERROR_ACCESS_DENIED: i32 = 5;

        return match Listener::bind(socket) {
            Ok(listener) => Ok(listener),
            Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                Err(BootError::AlreadyRunning {
                    pid: read_pidfile(paths)?,
                })
            }
            Err(source) => Err(BootError::Io {
                path: socket.to_path_buf(),
                source,
            }),
        };
    }

    #[cfg(unix)]
    {
        // Ahead of the bind, because the kernel's refusal names neither the
        // limit nor `$SHEP_HOME`. `sun_path` holds a NUL terminator, so the
        // usable length is one less.
        const SUN_PATH_CAPACITY: usize = if cfg!(target_os = "linux") { 108 } else { 104 };
        let len = socket.as_os_str().as_encoded_bytes().len();
        if len >= SUN_PATH_CAPACITY {
            return Err(BootError::SocketPathTooLong {
                path: socket.to_path_buf(),
                len,
                limit: SUN_PATH_CAPACITY - 1,
            });
        }
        warn_if_socket_dir_is_loose(socket);
        match Listener::bind(socket) {
            Ok(listener) => Ok(listener),
            Err(err) if err.kind() == ErrorKind::AddrInUse => {
                // EADDRINUSE only says the path exists. Only a refusal is
                // proof of absence: a dying daemon's forked child keeps the
                // socket answering until its close-on-exec clears, so an
                // answer refuses this boot rather than proving a live peer.
                match std::os::unix::net::UnixStream::connect(socket) {
                    Ok(_) => Err(BootError::AlreadyRunning {
                        pid: read_pidfile(paths)?,
                    }),
                    Err(probe)
                        if matches!(
                            probe.kind(),
                            ErrorKind::ConnectionRefused | ErrorKind::NotFound
                        ) =>
                    {
                        std::fs::remove_file(socket).map_err(|source| BootError::Io {
                            path: socket.to_path_buf(),
                            source,
                        })?;
                        Listener::bind(socket).map_err(|source| BootError::Io {
                            path: socket.to_path_buf(),
                            source,
                        })
                    }
                    Err(source) => Err(BootError::Io {
                        path: socket.to_path_buf(),
                        source,
                    }),
                }
            }
            Err(source) => Err(BootError::Io {
                path: socket.to_path_buf(),
                source,
            }),
        }
    }
}

/// Environment variable naming the inherited readiness descriptor.
///
/// Set by the CLI on the child it re-execs detached, and adopted by that same
/// CLI through `crate::sys::adopt_fd`. shep-daemon never parses it or sees a
/// raw fd, only the adopted [`std::fs::File`] in [`BootOptions::ready_fd`](super::BootOptions::ready_fd).
pub const READY_FD_ENV: &str = "SHEP_READY_FD";

/// What the daemonizing parent reads off the readiness pipe.
///
/// Crate-private, unlike [`READY_FD_ENV`]: the CLI-side reader deserializes
/// into a struct of its own, so the wire format is the contract rather than
/// this type.
// wire format: shep-cli parses this line; changing it is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DaemonReady {
    /// This daemon's OS pid.
    pub(crate) pid: u32,
    /// This daemon's crate version.
    pub(crate) version: String,
}

/// Writes one newline-terminated JSON readiness line to `pipe` and closes it.
/// Dropping `pipe` here is the parent's EOF.
///
/// # Errors
/// - [`BootError::ReadyWrite`] if the write failed, carrying the OS error.
pub(super) fn write_ready(mut pipe: std::fs::File, ready: &DaemonReady) -> Result<(), BootError> {
    use std::io::Write;

    // `DaemonReady` is a plain {u32, String} pair, and `to_string` fails only
    // on non-string map keys and NaN floats.
    let mut line = serde_json::to_string(ready).expect("DaemonReady always serializes");
    line.push('\n');
    pipe.write_all(line.as_bytes())
        .map_err(BootError::ReadyWrite)?;
    Ok(())
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::boot::{init_dirs, paths_in};

    /// `#[tokio::test]`, unlike the Windows tier's other cases: creating a named pipe
    /// instance registers it with the tokio reactor, so
    /// `ServerOptions::create` panics outside a runtime context.
    #[tokio::test]
    async fn a_second_bind_on_a_live_control_address_reports_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        init_dirs(&paths).unwrap();

        let _live = bind_socket(&paths, &paths.socket).expect("the first bind must succeed");

        let refusal = bind_socket(&paths, &paths.socket)
            .expect_err("a second daemon must not bind the same pipe");
        assert!(
            matches!(refusal, BootError::AlreadyRunning { .. }),
            "a taken pipe name must read as AlreadyRunning, got {refusal:?}"
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::boot::pidfile::write_pidfile;
    use crate::boot::{BootOptions, SIGNAL_TEST_LOCK, boot, init_dirs};
    use crate::fake::ScriptedRunner;
    use crate::testing::test_paths;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn socket_path_honors_a_config_override() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        assert_eq!(socket_path(&paths, None), paths.socket);
        let custom = dir.path().join("custom.sock");
        assert_eq!(socket_path(&paths, Some(&custom)), custom);
    }

    /// The kernel's `ENAMETOOLONG` names neither the limit nor `$SHEP_HOME`,
    /// and the limit is 104 here, 108 on Linux.
    #[tokio::test]
    async fn an_over_length_socket_path_names_the_limit_and_the_variable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        // Comfortably past both platforms' capacity, without depending on
        // which one this is.
        let long = dir.path().join("x".repeat(200));

        let err = bind_socket(&paths, &long).expect_err("a path this long cannot bind");
        assert!(
            matches!(err, BootError::SocketPathTooLong { .. }),
            "refused before the syscall, not translated after it: {err:?}"
        );

        let rendered = err.to_string();
        assert!(
            rendered.contains("$SHEP_HOME"),
            "the message names what to shorten: {rendered}"
        );
        assert!(
            rendered.contains("bytes"),
            "and the limit it is measured against: {rendered}"
        );
        assert!(
            !rendered.contains('\u{2014}') && !rendered.contains('\u{2013}'),
            "no em or en dash in copy a user reads: {rendered}"
        );
    }

    #[tokio::test]
    async fn bind_socket_binds_a_fresh_path() {
        // Real time: real socket IO (see the paused-clock rule).
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let listener = bind_socket(&paths, &paths.socket).unwrap();
        assert!(paths.socket.exists());
        drop(listener);
    }

    /// Fabricates what a crashed daemon leaves behind: a socket file at
    /// `socket` that nothing is listening on. Bind-then-drop is the shape,
    /// since neither std nor tokio unlinks the path, but macOS marks the
    /// descriptor close-on-exec just after `socket(2)` returns, so a child
    /// another test forks in that window holds a duplicate and the path keeps
    /// answering. So this waits until the path refuses a connection; only a
    /// fresh bind can undo that. Real sleeps, not the module's paused clock:
    /// another process's descriptor is on no clock tokio can advance.
    ///
    /// # Panics
    /// If the leftover never goes stale, or the probe fails for any reason
    /// other than nobody listening.
    #[track_caller]
    fn stale_socket_leftover(socket: &Path) {
        // Two loops for two holders: the inner waits out a child that copied
        // the descriptor mid-spawn and drops it on `exec`; the outer
        // re-fabricates for one that forked inside the close-on-exec window
        // and keeps it for life, since unlinking detaches that socket for good.
        for _ in 0..20 {
            let _ = std::fs::remove_file(socket);
            drop(std::os::unix::net::UnixListener::bind(socket).unwrap());
            for _ in 0..40 {
                match std::os::unix::net::UnixStream::connect(socket) {
                    Err(refused)
                        if matches!(
                            refused.kind(),
                            ErrorKind::ConnectionRefused | ErrorKind::NotFound
                        ) =>
                    {
                        return;
                    }
                    Ok(_) => std::thread::sleep(Duration::from_millis(5)),
                    Err(other) => {
                        panic!("probing the fabricated leftover socket failed: {other}")
                    }
                }
            }
        }
        panic!(
            "{} never went stale: something kept answering on it",
            socket.display()
        );
    }

    #[tokio::test]
    async fn a_socket_left_by_a_crash_is_unlinked_and_rebound() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        stale_socket_leftover(&paths.socket);
        assert!(paths.socket.exists(), "the stale file must still be there");
        let listener = bind_socket(&paths, &paths.socket).unwrap();
        assert!(paths.socket.exists());
        drop(listener);
    }

    #[tokio::test]
    async fn a_live_socket_is_reported_as_already_running() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // The raw unix type, not `shep_core::transport::Listener`: what this
        // proves is that a real socket someone else listens on reads as
        // `AlreadyRunning`, not anything about shep's own wrapper.
        let live = tokio::net::UnixListener::bind(&paths.socket).unwrap();
        write_pidfile(&paths, 4242).unwrap();
        assert!(matches!(
            bind_socket(&paths, &paths.socket),
            Err(BootError::AlreadyRunning { pid: Some(4242) })
        ));
        // A pure probe: the live daemon's socket file is untouched and still
        // answers, so `bind_socket` never reached the remove_file/rebind arm.
        assert!(
            paths.socket.exists(),
            "a live daemon's socket must never be unlinked"
        );
        std::os::unix::net::UnixStream::connect(&paths.socket)
            .expect("the live listener must still be accepting after a refused bind");
        drop(live);
    }

    /// What one racer thread observed in
    /// [`two_concurrent_boots_on_a_stale_socket_exactly_one_wins`]
    ///
    /// Small and `'static` so it crosses the thread boundary without carrying
    /// a `RunningDaemon`, and the tokio resources in it, out of the runtime
    /// that created it.
    #[derive(Debug)]
    enum RaceOutcome {
        Won { socket_still_accepts: bool },
        AlreadyRunning,
        Other(String),
    }

    #[test]
    fn two_concurrent_boots_on_a_stale_socket_exactly_one_wins() {
        // Two daemons racing on a crashed predecessor's leftover both see
        // `ConnectionRefused` and enter `bind_socket`'s recovery arm, where the
        // loser's `remove_file` can delete the winner's fresh listener.
        // Looped: the bad interleaving does not land on every attempt.
        for _ in 0..25 {
            // `blocking_lock` per SIGNAL_TEST_LOCK's rule: this fn is sync.
            let _guard = SIGNAL_TEST_LOCK.blocking_lock();
            let dir = tempfile::tempdir().unwrap();
            let paths = test_paths(&dir);
            init_dirs(&paths).unwrap();
            // Must really be leftover before the racers start: a socket kept
            // briefly alive by another test's child would make the winner
            // refuse too, for a reason unrelated to this race.
            stale_socket_leftover(&paths.socket);

            // Real OS threads on a barrier, not tokio tasks: `boot`'s
            // synchronous prefix never awaits, so two tasks on one runtime
            // would run one body to completion before the other was scheduled.
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let paths = paths.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait(); // both racers cross together
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(async {
                            match boot(ScriptedRunner::new(vec![]), paths, BootOptions::default())
                                .await
                            {
                                Ok(daemon) => {
                                    // Checked inside this racer's own
                                    // runtime: nothing tokio-shaped crosses
                                    // the thread boundary.
                                    let reachable =
                                        std::os::unix::net::UnixStream::connect(daemon.socket())
                                            .is_ok();
                                    RaceOutcome::Won {
                                        socket_still_accepts: reachable,
                                    }
                                }
                                Err(BootError::AlreadyRunning { .. }) => {
                                    RaceOutcome::AlreadyRunning
                                }
                                Err(other) => RaceOutcome::Other(other.to_string()),
                            }
                        })
                    })
                })
                .collect();
            let outcomes: Vec<RaceOutcome> =
                handles.into_iter().map(|h| h.join().unwrap()).collect();

            for outcome in &outcomes {
                if let RaceOutcome::Other(msg) = outcome {
                    panic!("a racer hit neither Ok nor AlreadyRunning: {msg}");
                }
            }

            let wins = outcomes
                .iter()
                .filter(|o| matches!(o, RaceOutcome::Won { .. }))
                .count();
            let already_running = outcomes
                .iter()
                .filter(|o| matches!(o, RaceOutcome::AlreadyRunning))
                .count();
            assert_eq!(
                wins, 1,
                "exactly one racer must win a boot on the same $SHEP_HOME: {outcomes:?}"
            );
            assert_eq!(
                already_running, 1,
                "the loser must be refused as AlreadyRunning, not silently succeed or hit some other error: {outcomes:?}"
            );
            assert!(
                matches!(
                    outcomes
                        .iter()
                        .find(|o| matches!(o, RaceOutcome::Won { .. }))
                        .unwrap(),
                    RaceOutcome::Won {
                        socket_still_accepts: true
                    }
                ),
                "the winner's own socket must still accept a connection, proving its bind \
                 wasn't the one the loser's remove_file clobbered: {outcomes:?}"
            );
        }
    }

    #[test]
    fn readiness_reports_pid_and_version_then_closes_the_pipe() {
        use std::io::Read;
        // No `unsafe`: `std::io::pipe` hands back an owned `PipeWriter`, which
        // converts into `File` through the standard `OwnedFd` bridge.
        let (mut reader, writer) = std::io::pipe().unwrap();
        let pipe = std::fs::File::from(std::os::fd::OwnedFd::from(writer));
        let ready = DaemonReady {
            pid: 4242,
            version: "0.1.0".to_string(),
        };
        write_ready(pipe, &ready).unwrap();
        let mut line = String::new();
        reader.read_to_string(&mut line).unwrap();
        assert_eq!(line.trim_end(), serde_json::to_string(&ready).unwrap());
        assert!(line.ends_with('\n'), "the parent reads a line: {line:?}");
    }
}
