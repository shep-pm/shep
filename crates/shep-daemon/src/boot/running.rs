//! The booted daemon: what `boot` hands back, and how it serves and tears down
//!
//! [`RunningDaemon`] is everything assembled and nothing yet serving, so a
//! caller can take [`RunningDaemon::context`] before driving
//! [`RunningDaemon::run`]. That teardown order is load-bearing, and `run`'s
//! own doc carries the reasons: the final roll is written before anything is
//! stopped, and every step runs however serving ended.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;
use shep_core::protocol::BusEvent;
use shep_core::transport::Listener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::BootError;
use super::pidfile::{PidfileLock, pidfile};
use super::signals::SignalTasks;
use crate::bus::SharedEvent;
use crate::rpc::RpcContext;
use crate::server::RpcServer;
use crate::snapshot::SnapshotWriter;

/// A booted daemon, not yet serving: everything [`boot`](super::boot) assembled, handed
/// back so the caller can read [`Self::context`] before driving [`Self::run`].
#[derive(Debug)]
pub struct RunningDaemon {
    pub(super) ctx: RpcContext,
    pub(super) listener: Listener,
    pub(super) writer: SnapshotWriter,
    // Held rather than detached: `run`'s teardown step 1 aborts both, so
    // nothing rewrites the roll or asks for a dog's restart once serving ends.
    pub(super) dog_watch: JoinHandle<()>,
    pub(super) silent_dog_watch: JoinHandle<()>,
    pub(super) paths: ShepPaths,
    pub(super) socket: PathBuf,
    // Held from `boot`, not resubscribed in `run`: `watch::Sender::send` is a
    // silent no-op at zero receivers, and `ctx.shutdown()` is callable the
    // moment `boot` returns, so a gap here loses that signal forever.
    pub(super) shutdown_rx: watch::Receiver<bool>,
    // Kept alive through `run`'s whole serving lifetime; `SignalTasks`'s
    // `Drop` is what stops these tasks.
    pub(super) signals: SignalTasks,
    // Dropping this `flock` is what lets the next daemon's own
    // `PidfileLock::acquire` succeed.
    pub(super) pidfile_lock: PidfileLock,
    pub(super) delete_flock_on_shutdown: bool,
}

impl RunningDaemon {
    /// Handles for driving this daemon from outside its run loop.
    #[must_use]
    pub fn context(&self) -> RpcContext {
        self.ctx.clone()
    }

    /// The control socket this daemon is bound to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Serves until a signal or `KillDaemon`, then tears down in order
    ///
    /// Every teardown step runs unconditionally: stop the snapshot writer and
    /// both dog watches, write the final muster roll, broadcast
    /// [`BusEvent::DaemonShutdown`] before subscribers' sockets close, stop
    /// the flock in reverse dependency order, run
    /// [`SupervisorHandle::shutdown`](crate::supervisor::SupervisorHandle::shutdown)'s kill ladder, then unlink the socket and
    /// the pidfile best-effort. The roll goes before every stop, and the writer
    /// is stopped before the roll, or their `Exit`/`Stop` events leave a
    /// roll of stopped sheep for `shep muster` to restore nothing from.
    ///
    /// # Errors
    /// - [`BootError::Io`] if a teardown filesystem step failed.
    pub async fn run(self) -> Result<(), BootError> {
        let RunningDaemon {
            ctx,
            listener,
            writer,
            dog_watch,
            silent_dog_watch,
            paths,
            socket,
            shutdown_rx,
            // Bound, not dropped with `_`: both must outlive the serving
            // lifetime below, and only their `Drop` at the end of this scope
            // stops the signal tasks and releases the home.
            signals: _signals,
            pidfile_lock: _pidfile_lock,
            delete_flock_on_shutdown,
        } = self;

        // The receiver `boot` kept alive, reused rather than a fresh
        // `ctx.shutdown.subscribe()`: no window with zero receivers between
        // `boot` returning and this line running.
        RpcServer::new(listener, ctx.clone())
            .serve(shutdown_rx)
            .await;

        // 1. Nothing may rewrite the roll or ask for a dog's restart from here
        //    on.
        writer.stop().await;
        dog_watch.abort();
        silent_dog_watch.abort();

        // 2. The final roll, written while every sheep is still online, unless
        //    `delete_flock_on_shutdown` (`shep dev`'s case) wiped the registry
        //    first. Runs however serving ended, a caught signal included.
        //
        //    The edges are read BEFORE the clear, because step 4 needs them
        //    and the clear is about what a roll persists, not about how a
        //    session tears down: a `shep dev` worker drains against its
        //    database the same as any other, and reading them after the clear
        //    left that session with no ordered teardown at all.
        let edges = ctx.registry.depends_on_by_name();
        if delete_flock_on_shutdown {
            ctx.registry.clear();
        }
        if let Err(err) = ctx.snapshot_now().await {
            tracing::warn!(%err, "final muster roll write failed");
        }

        // 3. Tell subscribers before their sockets close underneath them.
        let _ = ctx.events.send(SharedEvent::new(BusEvent::DaemonShutdown));

        // 4. Stop the flock in reverse dependency order, so a worker drains
        //    against a database that is still answering. Every sheep is
        //    bounded by its own kill ladder, and step 5 is the backstop: a
        //    sheep this walk misses is still killed there, so a bug here
        //    cannot leave a child alive.
        crate::boot_order::stop_edges_in_reverse(edges, &ctx.dog_names, &ctx.supervisor).await;

        // 5. Kill ladder on whatever is still online, dogs included: they are
        //    deliberately not in the reverse stages above, because monitoring
        //    should outlive what it monitors.
        ctx.supervisor.shutdown().await;

        // 6. Both are attempted regardless and the first failure wins, so a
        // socket-unlink error cannot hide a pidfile nothing tried to remove.
        // Unix only: `remove_file` on a `\.\pipe\...` name fails with
        // `ERROR_INVALID_PARAMETER` and would fail every Windows shutdown.
        #[cfg(unix)]
        let unlink_socket = unlink_if_present(&socket);
        #[cfg(windows)]
        let unlink_socket = {
            let _ = &socket;
            Ok(())
        };
        let unlink_pidfile = unlink_if_present(&pidfile(&paths));
        unlink_socket.and(unlink_pidfile)
    }
}

/// Removes `path`, treating "already gone" as success: teardown's job is to
/// make sure it is gone, not to prove it was there.
fn unlink_if_present(path: &Path) -> Result<(), BootError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(BootError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::boot::pidfile::read_pidfile;
    use crate::boot::{BootOptions, SIGNAL_TEST_LOCK, boot, init_dirs};
    use crate::dogs::DogSpec;
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::snapshot::{FlockSnapshot, SNAPSHOT_VERSION, SavedApp};
    use crate::testing::test_paths;
    use shep_core::config::AppConfig;
    use shep_core::protocol::{DogSource, ProcessEventKind};
    use std::time::Duration;

    #[tokio::test]
    async fn boot_restores_a_saved_flock_and_tears_down_in_order() {
        // Real time: binds a real socket. Locked per SIGNAL_TEST_LOCK's rule.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        // A wrong `instances_running` on purpose: restore reads
        // `app.instances`, not this count, so 99 changes nothing about the
        // boot and survives untouched if teardown's roll write is skipped. A
        // seeded 1 would have matched the right value by coincidence.
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("web", "./srv"),
                instances_running: 99,
            }],
        };
        crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

        let daemon = boot(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                restore: true,
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let flock = ctx.supervisor.list_checked().await.unwrap();
        assert_eq!(flock.len(), 1, "the muster roll must be back on its feet");
        assert_eq!(flock[0].name, "web");

        let run = tokio::spawn(daemon.run());
        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        // The roll written during teardown records the flock as it WAS.
        let final_roll = crate::snapshot::read(&paths.snapshot).unwrap();
        assert_eq!(
            final_roll.apps[0].instances_running, 1,
            "the roll must be written before the flock is killed, or muster restores nothing"
        );
        assert!(
            !paths.socket.exists(),
            "the socket is unlinked on a clean exit"
        );
        assert_eq!(read_pidfile(&paths).unwrap(), None);
    }

    /// Pins the shared shutdown watch: `ctx.shutdown()` flips the same watch
    /// `install_signals` flips on a caught SIGTERM, with no caller-level
    /// `Stop`/`Delete` first, which is the gap a CLI-side `tidy_up` flag
    /// cannot close. It raises no real signal, so it says nothing about the
    /// listener wiring.
    #[tokio::test]
    async fn delete_flock_on_shutdown_clears_the_roll_even_on_a_signalled_exit() {
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        let daemon = boot(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                delete_flock_on_shutdown: true,
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let held = shep_core::config::normalize(AppConfig::minimal("held", "./held")).unwrap();
        ctx.registry.record(std::slice::from_ref(&held));
        ctx.supervisor.start(vec![held]).await.unwrap();
        let flock = ctx.supervisor.list_checked().await.unwrap();
        assert_eq!(flock.len(), 1, "the held app must actually be up");

        let run = tokio::spawn(daemon.run());
        // The signal path, not a caller-level `Stop`/`Delete` pair: `run`'s
        // `install_signals` handler flips this same watch on a real `SIGTERM`.
        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let final_roll = crate::snapshot::read(&paths.snapshot).unwrap();
        assert!(
            final_roll.apps.is_empty(),
            "delete_flock_on_shutdown must leave the roll empty, not {:?}",
            final_roll.apps
        );
    }

    #[tokio::test]
    async fn a_dev_session_still_stops_its_flock_in_dependency_order() {
        // fails if the reverse walk reads its edges after the registry is
        // cleared: `delete_flock_on_shutdown` empties it before the final
        // roll, so the walk planned nothing and the backstop killed the
        // whole flock at once. A dev worker drains against its database the
        // same as any other, and not persisting a roll is a different thing
        // from not draining.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        let daemon = boot(
            // `worker` ignores signals, so its stop has a kill ladder to
            // burn while `db` answers at once. Without that the two `Stop`
            // events land in poll order, which matches stage order by
            // accident whether the walk kept its stages or not.
            ScriptedRunner::new(vec![
                ProcScript::never_exits(),
                ProcScript::ignores_signals(),
            ]),
            paths.clone(),
            BootOptions {
                delete_flock_on_shutdown: true,
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let db = AppConfig::minimal("db", "./db");
        let mut worker = AppConfig::minimal("worker", "./worker");
        worker.depends_on = vec!["db".to_string()];
        // Short enough that the ladder is a margin the assertion can read
        // rather than a second and a half of test.
        worker.kill_timeout = shep_core::values::UpDuration::from_millis(100);
        let apps = shep_core::config::normalize_all(vec![db, worker]).unwrap();
        ctx.registry.record(&apps);
        ctx.supervisor.start(apps).await.unwrap();

        let mut rx = ctx.events.subscribe();
        let run = tokio::spawn(daemon.run());
        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let mut stops = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let shep_core::protocol::BusEvent::Process {
                event: shep_core::protocol::ProcessEventKind::Stop,
                info,
                ..
            } = &*event
            {
                stops.push(info.name.clone());
            }
        }
        assert_eq!(
            stops,
            vec!["worker".to_string(), "db".to_string()],
            "the dependant stops before what it waits on"
        );
    }

    /// Pins teardown's step 5, the `shutdown` backstop, which nothing else
    /// does: deleting that line leaves every other test in this crate green.
    ///
    /// A dog is what makes it observable. The reverse walk skips dogs
    /// deliberately, so monitoring outlives what it monitors, which leaves the
    /// backstop as the only thing in teardown that can stop one. The same
    /// call is what the step's own comment promises for a sheep the walk
    /// misses.
    #[tokio::test]
    async fn a_dog_is_stopped_by_the_time_run_returns() {
        // fails if teardown loses its kill-ladder backstop, which would leave
        // every dog running after a clean shutdown and a sheep the reverse
        // walk missed alive with nothing supervising it
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        let daemon = boot(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                dogs: vec![DogSpec {
                    name: "metrics".to_string(),
                    source: DogSource::BuiltIn,
                }],
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let flock = ctx.supervisor.list_checked().await.unwrap();
        assert_eq!(
            flock.len(),
            1,
            "the dog has to be up for its stop to mean anything: {flock:?}"
        );

        // Subscribed before `run`, since the actor is gone by the time it
        // returns and the flock cannot be read back.
        let mut rx = ctx.events.subscribe();
        let run = tokio::spawn(daemon.run());
        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        let mut stops = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let BusEvent::Process {
                event: ProcessEventKind::Stop,
                info,
                ..
            } = &*event
            {
                stops.push(info.name.clone());
            }
        }
        assert!(
            stops.iter().any(|name| name == "metrics"),
            "the dog must be stopped before run() returns; stops were {stops:?}"
        );
    }
}
