//! Daemon boot: layout, pidfile, control-socket bind, and the run/teardown
//! sequence
//!
//! `init_dirs` creates every layout directory `0700` and tightens one that
//! already exists looser, the guarantee `crate::server::RpcServer`'s doc names
//! as boot's. [`boot`] assembles bus, supervisor, muster roll and RPC context
//! into one [`RunningDaemon`]; [`RunningDaemon::run`] serves until a signal or
//! `KillDaemon`, then tears down in a load-bearing order.
//!
//! [`BootOptions::ready_fd`] arrives as an owned [`std::fs::File`]:
//! `crate::sys::adopt_fd`'s ordering precondition is process-wide and `boot`
//! is `async`, so only the CLI's `main` can discharge it.

mod error;
#[cfg(unix)]
mod handover;
mod layout;
mod pidfile;
mod signals;
mod socket;

pub use self::error::BootError;
pub use self::layout::DIR_MODE;
pub(crate) use self::layout::init_dirs;
pub use self::pidfile::{Shepherd, daemon_liveness, pidfile};
pub use self::socket::READY_FD_ENV;

use core::time::Duration;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shep_core::transport::Listener;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use shep_core::paths::ShepPaths;
use shep_core::protocol::BusEvent;

#[cfg(unix)]
use self::handover::{HandoverSeam, apps_for_the_roll, rehydrate, successor_handover};
use self::pidfile::PidfileLock;
use self::signals::{SignalTasks, install_signals};
use self::socket::{DaemonReady, bind_socket, socket_path, write_ready};
use crate::bus::{Bus, SharedEvent, new_bus};
use crate::cron::DEFAULT_MAX_CRON_SLEEP;
use crate::dogs::{DogSpec, spawn_dog_watch};
use crate::extras::{Extras, ExtrasReports, spawn_extras_reporter};
use crate::rpc::RpcContext;
use crate::runner::ProcessRunner;
use crate::server::RpcServer;
use crate::snapshot::{self, FlockRegistry, SnapshotWriter, spawn_snapshot_writer};
use crate::supervisor::{SupervisorBuilder, SupervisorHandle};

/// Capacity of each of the two lifecycle-extra report channels.
///
/// Bounded, so a report producer that outruns the reporting task
/// back-pressures instead of queueing restarts nobody has performed. 64 fits
/// a whole flock breaching on one sampling pass.
const EXTRAS_REPORT_CAPACITY: usize = 64;

/// Options the CLI hands the daemon at boot.
#[derive(Debug, Default)]
pub struct BootOptions {
    /// Overrides the layout's default control-socket path.
    pub socket: Option<PathBuf>,
    /// The inherited readiness pipe (see [`READY_FD_ENV`]), adopted into an
    /// owned [`std::fs::File`] by the caller.
    ///
    /// Adoption is the caller's job: `crate::sys::adopt_fd`'s precondition,
    /// "call before the process opens any descriptor of its own", is
    /// process-wide, and [`boot`] already runs inside a tokio runtime with
    /// poller fds of its own.
    pub ready_fd: Option<std::fs::File>,
    /// Restore the muster roll if one exists.
    pub restore: bool,
    /// Longest a cron worker parks before re-reading the wall clock, from
    /// `[daemon] max_cron_sleep`. Unset means `DEFAULT_MAX_CRON_SLEEP`,
    /// which [`boot`] applies and nothing else does.
    pub max_cron_sleep: Option<Duration>,
    /// Where to report readiness once the muster restore has finished, for an
    /// init system supervising this process directly. `None` reports nothing.
    ///
    /// The resolved address rather than a bool: `std::env::set_var` is
    /// `unsafe` in edition 2024 and this crate is `#![deny(unsafe_code)]`, so
    /// a boot test could not establish an ambient `$NOTIFY_SOCKET`.
    ///
    /// Distinct from [`Self::ready_fd`], which answers a parent shep process
    /// the moment the socket binds. This one is written last, so a unit goes
    /// green only once the flock is back.
    pub notify_socket: Option<OsString>,
    /// Dogs to start once the flock is back, in the order given.
    ///
    /// Assembled by the caller from `[daemon] enabled_dogs` and
    /// `[daemon] adopted_dogs`, so shep-daemon never reads `shep.toml` itself.
    pub dogs: Vec<DogSpec>,
    /// Every dog name this shepherd may hold a section for, running or
    /// not: the built-in dogs plus every name `[daemon] adopted_dogs`
    /// records, plus whatever `[daemon] enabled_dogs` names.
    ///
    /// Assembled by the caller from the same file [`Self::dogs`] comes out
    /// of, and for the same reason: shep-daemon never reads `shep.toml`
    /// itself.
    ///
    /// A superset of [`Self::dogs`], and the difference is the whole point
    /// of carrying both. That one is the spawn list, so it holds only the
    /// dogs an operator has switched on; this one holds the dogs that
    /// exist. `Request::SetDogConfig` is guarded on this one, because the
    /// dog most in need of configuring is the one that is disabled or has
    /// never started, and a guard on the running set refuses exactly that
    /// dog.
    pub known_dogs: Vec<String>,
    /// Which of [`Self::dogs`] run before every sheep rather than after the
    /// flock, from `[daemon] boot_first_dogs`.
    ///
    /// Assembled by the caller from the same file [`Self::dogs`] comes out
    /// of, and for the same reason: shep-daemon never reads `shep.toml`
    /// itself.
    pub boot_first_dogs: Vec<String>,
    /// Wipe the in-memory flock registry before [`RunningDaemon::run`]'s
    /// teardown writes the final muster roll, so that roll describes an empty
    /// flock however the session ended.
    ///
    /// `true` only for `shep dev`'s isolated session; a real boot needs the
    /// roll to carry the flock's running state for `shep muster`.
    pub delete_flock_on_shutdown: bool,
    /// Let SIGHUP replace this process's image with a successor holding the
    /// same flock, rather than stopping gracefully.
    ///
    /// A handover `execve`s the file this process was launched from, so a
    /// caller that opts in is asserting it is the shep binary; a test harness
    /// would re-run itself from the top forever. Defaults to `false`, and a
    /// boot that has not opted in answers SIGHUP with the graceful stop, as
    /// does a handover that cannot proceed. Unix only in effect.
    pub handover: bool,
    /// The environment a sheep that names none of its own resolves its
    /// `{{secret:...}}` references in, from `[daemon] environment`.
    ///
    /// Assembled by the caller from the same file every other field here
    /// comes out of: shep-daemon never reads `shep.toml` itself. `None`
    /// takes the supervisor's own default, which is the one
    /// `DaemonSection` applies when the file says nothing.
    pub environment: Option<String>,
}

/// Brings the daemon up: layout, lock, socket, dogs, restore, dogs, readiness
///
/// The order is load-bearing: handlers before the socket (SIGUSR2 otherwise
/// terminates), the pidfile lock before the bind it makes race-free,
/// `ready_fd` on the bind not the restore, `[daemon] boot_first_dogs` before
/// the restore and every other dog after it so a metrics dog does not answer
/// for an empty flock, [`BootOptions::notify_socket`] last.
///
/// # Errors
/// - [`BootError::Io`] if a boot filesystem or signal-handler step failed.
/// - [`BootError::AlreadyRunning`] if another daemon holds the lock or answered.
/// - [`BootError::ReadyWrite`] if the readiness line could not be written.
/// - [`BootError::Snapshot`] if a roll exists and could not be read or parsed.
pub async fn boot<R: ProcessRunner>(
    runner: R,
    paths: ShepPaths,
    mut options: BootOptions,
) -> Result<RunningDaemon, BootError> {
    // Before anything else: it reads the current directory, which is the
    // startup directory only until something moves it.
    #[cfg(unix)]
    crate::handover::record_launch_path();

    let delete_flock_on_shutdown = options.delete_flock_on_shutdown;

    // 1. Signal handlers, before the socket or anything else observable
    //    exists.
    let (shutdown, shutdown_rx) = watch::channel(false);
    let shutdown = Arc::new(shutdown);
    #[cfg(unix)]
    let (signals, connect_supervisor, connect_handover) =
        install_signals(Arc::clone(&shutdown), paths.clone())?;
    #[cfg(windows)]
    let (signals, connect_supervisor) = install_signals(Arc::clone(&shutdown), paths.clone())?;

    // 2. Layout, then claim $SHEP_HOME before touching the socket: that is
    //    what closes the concurrent-boot race a bare probe-then-recover
    //    sequence cannot. Held for the rest of this daemon's life.
    init_dirs(&paths)?;
    let socket = socket_path(&paths, options.socket.as_deref());
    // A successor takes neither the lock nor the address: it inherited both,
    // still held. Rebinding would race the predecessor's own socket file, and
    // re-locking would mean releasing first.
    #[cfg(unix)]
    let (mut pidfile_lock, listener, inherited) = match successor_handover() {
        Some(carried) => {
            let (lock, listener, flock) = rehydrate(carried, &paths)?;
            (lock, listener, Some(flock))
        }
        None => (
            PidfileLock::acquire(&paths)?,
            bind_socket(&paths, &socket)?,
            None,
        ),
    };
    #[cfg(windows)]
    let (mut pidfile_lock, listener) =
        (PidfileLock::acquire(&paths)?, bind_socket(&paths, &socket)?);
    let pid = std::process::id();
    pidfile_lock.record(&paths, pid)?;

    // 3. Readiness, now that the socket is bound. Taken rather than moved out:
    //    a partial move leaves `options` unborrowable, and step 4 hands it
    //    whole to `max_cron_sleep`.
    if let Some(pipe) = options.ready_fd.take() {
        let ready = DaemonReady {
            pid,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        write_ready(pipe, &ready)?;
    }

    // 4. Bus, supervisor, muster restore, snapshot writer, context.
    let events = new_bus();
    // Subscribed before the supervisor that emits onto the bus, so it cannot
    // miss an `Errored` a dog reaches during the restore step.
    let dog_watch = spawn_dog_watch(events.subscribe(), events.clone(), paths.barks.clone());
    let (breach_tx, breach_rx) = mpsc::channel(EXTRAS_REPORT_CAPACITY);
    let (live_tx, live_rx) = mpsc::channel(EXTRAS_REPORT_CAPACITY);
    let extras = Extras::real(
        ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        max_cron_sleep(&options),
    );
    // One `StatsState`, two owners: the extras record the periodic CPU
    // baseline and the RPC layer reads a live sample against it, so a second
    // state would leave one of them on an empty watch set.
    let stats = Arc::clone(&extras.stats);
    // One registry, two owners, as `stats` above: the connection tasks
    // serving `Request::PutSecrets` write it and the actor reads a view out
    // of it per spawn, so a second one would leave a dog pushing into a
    // registry no spawn ever consults.
    let provider_secrets = Arc::new(crate::secrets::ProviderSecrets::load(&paths.secrets_cache));
    // Names and environments, never values (IR-41). This is the line that
    // separates "the dog has not polled since the restart" from "the operator
    // turned the cache off", which a sheep held back by a `MissingNamespace`
    // cannot say.
    tracing::debug!(
        pushed = ?provider_secrets.pushed(),
        "provider namespaces restored from the secrets cache"
    );
    let mut builder = SupervisorBuilder::new(runner, paths.clone(), events.clone())
        .extras(extras)
        .provider_secrets(Arc::clone(&provider_secrets));
    if let Some(environment) = options.environment.take() {
        builder = builder.environment(environment);
    }
    // A successor installs the flock it inherited rather than spawning one:
    // every sheep keeps its pid, id, epoch and history, and nothing here
    // signals, spawns or reopens.
    #[cfg(unix)]
    let mut carried_apps = Vec::new();
    // Not derived from `carried_apps` below: a successor that inherited an
    // empty flock is still a successor.
    #[cfg(unix)]
    let inherited_flock = inherited.is_some();
    #[cfg(unix)]
    let supervisor = match inherited {
        Some((flock, counters, reloads)) => {
            // Read before the flock is moved: the registry below is rebuilt
            // from these, and the roll would otherwise be written empty.
            carried_apps.extend(apps_for_the_roll(&flock));
            builder
                .spawn_adopted(flock, counters, reloads)
                .map_err(|source| BootError::Adopt(source.to_string()))?
        }
        None => builder.spawn(),
    };
    #[cfg(windows)]
    let supervisor = builder.spawn();
    // The detached reporter and the actor hold each other alive: the reporter
    // holds a `SupervisorHandle`, and its own report senders live as long as
    // the actor's registry. Only `SupervisorHandle::shutdown` (teardown step
    // 4) ends either, so a teardown waiting on sender counts would hang.
    spawn_extras_reporter(breach_rx, live_rx, supervisor.clone());

    // The other half of step 1's SIGUSR2 listener, parked on this since before
    // the socket existed.
    let _ = connect_supervisor.send(supervisor.clone());

    // The other half of step 1's SIGHUP task. It carries the two descriptors a
    // handover blob has to name, which only this function knows: an fd number
    // means nothing outside the owning process.
    #[cfg(unix)]
    let _ = connect_handover.send(options.handover.then(|| HandoverSeam {
        supervisor: supervisor.clone(),
        fds: crate::handover::DaemonFds {
            listener: listener.as_raw_fd(),
            pidfile: pidfile_lock.as_raw_fd(),
        },
        paths: paths.clone(),
    }));

    let registry = FlockRegistry::new();

    // A successor rebuilds the registry from the blob and skips the restore.
    // An empty registry would overwrite a good roll within seconds, and a
    // restore would give a flock that never stopped a second copy of every
    // sheep the roll records as running.
    #[cfg(unix)]
    for app in &carried_apps {
        registry.record_config(app);
    }
    #[cfg(windows)]
    let inherited_flock = false;

    // Split around the restore rather than run whole after it: a log-rotation
    // dog has to be running before a sheep starts writing, while a metrics dog
    // must not answer for a flock that is not up. Both are true in one flock,
    // so `[daemon] boot_first_dogs` says which.
    let (first, rest): (Vec<DogSpec>, Vec<DogSpec>) = options
        .dogs
        .iter()
        .cloned()
        .partition(|spec| options.boot_first_dogs.contains(&spec.name));
    let dog_names: Vec<String> = options.dogs.iter().map(|dog| dog.name.clone()).collect();

    // Never fails the boot: a dog that cannot be spawned is a monitoring gap
    // rather than an outage, so `spawn_enabled_dogs` warns and carries on.
    crate::dogs::spawn_enabled_dogs(&first, &paths, &supervisor, &events).await;

    if options.restore && !inherited_flock {
        restore_flock(
            &paths,
            &registry,
            &supervisor,
            &events,
            &dog_names,
            &options.boot_first_dogs,
        )
        .await?;
    }

    crate::dogs::spawn_enabled_dogs(&rest, &paths, &supervisor, &events).await;

    // Starts empty and is not carried across a handover: a successor has
    // refused nobody.
    let dog_refusals = crate::dogs::DogRefusals::new();
    // Also not carried, so a successor must not claim a pid it has never seen
    // never called. `PEER_CONTACT_WARMUP` is what makes starting empty safe:
    // until the map has listened long enough for an absence to mean
    // something it answers `Contact::Unknown` rather than `Contact::None`.
    let peer_contacts = crate::dogs::PeerContacts::new();
    // Spawned at every boot, a successor's included, rather than anchored to a
    // dog's own spawn. It restarts a dog that has been running without ever
    // answering this shepherd; the tradeoff is argued at `record_silent_dog`.
    let silent_dog_watch = crate::dogs::spawn_silent_dog_watch(
        supervisor.clone(),
        dog_refusals.clone(),
        peer_contacts.clone(),
        events.clone(),
    );

    let writer = spawn_snapshot_writer(
        paths.snapshot.clone(),
        supervisor.clone(),
        registry.clone(),
        events.subscribe(),
    );

    let ctx = RpcContext {
        supervisor,
        events,
        registry,
        snapshot_path: paths.snapshot.clone(),
        dogs_config: paths.dogs_config.clone(),
        known_dogs: crate::rpc::KnownDogs::new(options.known_dogs.iter().cloned().collect()),
        dog_names,
        boot_first_dogs: options.boot_first_dogs.clone(),
        paths: paths.clone(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        dog_refusals,
        peer_contacts,
        pid,
        shutdown,
        stats,
        // Starts its own tick here rather than riding the supervisor's
        // extras: those are armed and disarmed per sheep, and this reads
        // the machine whether the flock is empty or not.
        host: crate::host::HostState::real(),
        provider_secrets,
    };

    // 5. A failure is a `warn!` and the boot continues: only systemd's view
    //    is wrong. Unix only, since `$NOTIFY_SOCKET` is a unix datagram
    //    socket; the field stays on `BootOptions` for both platforms.
    //
    //    Reported here, after the restore, which is the honest answer to
    //    `Type=notify` and is also what puts the staged restore inside
    //    systemd's `TimeoutStartSec`. The restore holds each stage until its
    //    members are ready, so the wait is a sum over stages: past the 90s
    //    default the shepherd is killed mid-restore and restarted on
    //    `RestartSec`. `startup::unit`'s `systemd_unit` carries the numbers
    //    and the argument for leaving the timeout to the operator.
    #[cfg(unix)]
    if let Some(target) = options.notify_socket.as_deref()
        && let Err(err) = crate::notify::notify(target)
    {
        tracing::warn!(
            %err,
            "readiness could not be reported to $NOTIFY_SOCKET; the flock is up regardless"
        );
    }

    Ok(RunningDaemon {
        ctx,
        listener,
        writer,
        dog_watch,
        silent_dog_watch,
        paths,
        socket,
        // `watch::Sender::send` is a silent no-op at zero receivers, and
        // `ctx.shutdown()` is callable the instant a caller has
        // `Self::context`, ahead of `run` ever being polled.
        shutdown_rx,
        signals,
        pidfile_lock,
        delete_flock_on_shutdown,
    })
}

/// The cron sleep bound this boot runs with: [`BootOptions::max_cron_sleep`],
/// or [`DEFAULT_MAX_CRON_SLEEP`] when `shep.toml` named none.
///
/// The one place that constant is applied: `shep-core` carries the floor and
/// never the default, the daemon carries the default and never the floor.
/// Named, and reading the whole [`BootOptions`], so a test has a seam to stand
/// on; the only behavioural trace is how often a cron worker wakes.
fn max_cron_sleep(options: &BootOptions) -> Duration {
    options.max_cron_sleep.unwrap_or(DEFAULT_MAX_CRON_SLEEP)
}

/// Reads the muster roll (if one exists) and starts every app it restores, in
/// dependency order.
///
/// One line over [`snapshot::muster`], which holds the whole restore rule and
/// also serves an operator's `Muster` request. The names it returns are
/// discarded: nobody is waiting on them here.
///
/// `dogs` is every dog this shepherd holds and `boot_first_dogs` the ones
/// promoted ahead of the flock. Neither is spawned here. Only the promoted
/// ones are running by the time a stage starts, so a sheep that names an
/// unpromoted dog in its `depends_on` is positioned by the plan and by
/// nothing else; both lists are passed on so the restore can warn about
/// exactly that.
///
/// This is where a boot's worst case grows with the shape of the flock. Every
/// stage holding a member something else depends on is waited for here, in
/// turn, before the daemon serves: up to that stage's longest
/// `listen_timeout` plus `boot_order::STAGE_SLACK`. A client connecting during
/// the restore waits with it. The bound is limited rather than open-ended,
/// since a stage nothing depends on is not waited for at all and a flock with
/// no `depends_on` anywhere pays nothing.
async fn restore_flock(
    paths: &ShepPaths,
    registry: &FlockRegistry,
    supervisor: &SupervisorHandle,
    events: &Bus,
    dogs: &[String],
    boot_first_dogs: &[String],
) -> Result<(), BootError> {
    snapshot::muster(
        &paths.snapshot,
        registry,
        supervisor,
        events,
        dogs,
        boot_first_dogs,
    )
    .await?;
    Ok(())
}

/// A booted daemon, not yet serving: everything [`boot`] assembled, handed
/// back so the caller can read [`Self::context`] before driving [`Self::run`].
#[derive(Debug)]
pub struct RunningDaemon {
    ctx: RpcContext,
    listener: Listener,
    writer: SnapshotWriter,
    // Held rather than detached: `run`'s teardown step 1 aborts both, so
    // nothing rewrites the roll or asks for a dog's restart once serving ends.
    dog_watch: JoinHandle<()>,
    silent_dog_watch: JoinHandle<()>,
    paths: ShepPaths,
    socket: PathBuf,
    // Held from `boot`, not resubscribed in `run`: `watch::Sender::send` is a
    // silent no-op at zero receivers, and `ctx.shutdown()` is callable the
    // moment `boot` returns, so a gap here loses that signal forever.
    shutdown_rx: watch::Receiver<bool>,
    // Kept alive through `run`'s whole serving lifetime; `SignalTasks`'s
    // `Drop` is what stops these tasks.
    signals: SignalTasks,
    // Dropping this `flock` is what lets the next daemon's own
    // `PidfileLock::acquire` succeed.
    pidfile_lock: PidfileLock,
    delete_flock_on_shutdown: bool,
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
    /// [`SupervisorHandle::shutdown`]'s kill ladder, then unlink the socket and
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

/// Serializes every test in this module tree whose `boot()` call succeeds
///
/// `raise(SIGTERM)` reaches every `Signal` stream in the test binary,
/// whichever runtime registered it, so two overlapping tests can rescue or
/// corrupt each other's daemon. A successful `boot()` is the line, not a
/// `run()`: `install_signals` runs inside `boot`.
///
/// `tokio::sync::Mutex`, since the guard is held across `.await`.
#[cfg(all(test, unix))]
static SIGNAL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A `$SHEP_HOME` under `dir`, for the Windows tier's tests anywhere in this
/// module tree.
///
/// Each file's `mod tests` is `#[cfg(unix)]` because almost every case in it
/// asserts something only unix has: a `0700` mode, a `raise(SIGTERM)`, a
/// socket file left behind by a crash. What survives translation is asserted
/// in a `windows_tests` beside it.
#[cfg(all(test, windows))]
fn paths_in(dir: &Path) -> ShepPaths {
    ShepPaths::resolve(
        &|key| (key == "SHEP_HOME").then(|| dir.to_string_lossy().into_owned()),
        Path::new(""),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::pidfile::read_pidfile;
    use super::*;
    use crate::dogs::DogSpec;
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::snapshot::{FlockSnapshot, SNAPSHOT_VERSION, SavedApp};
    use crate::testing::{AnnouncingRunner, capture_logs, test_paths};
    use shep_core::config::{AppConfig, ProbeConfig, ProbeKind, normalize};
    use shep_core::protocol::{DogSource, ProcessEventKind};
    use shep_core::status::ProcStatus;
    use shep_core::values::UpDuration;
    use std::time::Duration;

    #[tokio::test]
    async fn boot_writes_readiness_to_the_callers_pipe_after_the_socket_is_bound() {
        // Real time: binds a real socket. Locked per SIGNAL_TEST_LOCK's rule.
        // The only test driving a `Some` `ready_fd` through `boot`. A bad
        // descriptor cannot reach `BootOptions::ready_fd`, whose type is
        // `Option<std::fs::File>`, so `sys::tests` covers that refusal.
        use std::io::Read;
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        let (mut reader, writer) = std::io::pipe().unwrap();
        let pipe = std::fs::File::from(std::os::fd::OwnedFd::from(writer));

        let daemon = boot(
            ScriptedRunner::new(vec![]),
            paths.clone(),
            BootOptions {
                ready_fd: Some(pipe),
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();
        assert!(
            paths.socket.exists(),
            "boot must bind the socket before it returns"
        );

        // `write_ready` closes its `File`, so this read sees the line and then
        // EOF rather than blocking on a live writer.
        let mut line = String::new();
        reader.read_to_string(&mut line).unwrap();
        let ready: DaemonReady = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(ready.pid, std::process::id());
        assert!(line.ends_with('\n'), "the parent reads a line: {line:?}");

        drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
    }

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

    /// A metrics dog that starts first answers for an empty flock for the
    /// whole restore window, and a bark dog alerts on every restored sheep.
    /// [`ScriptedRunner`] hands out pids as `FIRST_SCRIPTED_PID + index`, so
    /// the spawn order is observable only as a pid order.
    #[tokio::test]
    async fn boot_restores_the_flock_before_it_lets_the_dogs_out() {
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("web", "./srv"),
                instances_running: 1,
            }],
        };
        crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

        let daemon = boot(
            // Two scripts: the restored sheep's spawn, then the dog's.
            ScriptedRunner::new(vec![ProcScript::never_exits(), ProcScript::never_exits()]),
            paths.clone(),
            BootOptions {
                restore: true,
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
        assert_eq!(flock.len(), 2, "the sheep and the dog must both be up");

        let sheep = flock
            .iter()
            .find(|p| p.name == "web")
            .expect("the restored sheep must be present");
        let dog = flock
            .iter()
            .find(|p| p.name == "metrics")
            .expect("the dog must be present");
        assert!(
            sheep.dog.is_none(),
            "the restored app must carry no dog marker"
        );
        assert_eq!(
            dog.dog,
            Some(DogSource::BuiltIn),
            "the dog entry must carry its source"
        );
        assert!(
            sheep.pid < dog.pid,
            "the sheep must be spawned before the dog: sheep={:?} dog={:?}",
            sheep.pid,
            dog.pid
        );

        drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
    }

    /// The dog gets no script, so `ScriptedRunner` answers
    /// `SpawnFailed("script exhausted")` on its spawn and the flock must still
    /// come up.
    ///
    /// `#[test]` with a `block_on` of its own, not `#[tokio::test]`:
    /// `capture_logs` scopes its subscriber to a synchronous closure.
    #[test]
    fn a_dog_that_will_not_start_does_not_fail_the_boot() {
        let _guard = SIGNAL_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut boot_result = None;
        let logs = capture_logs(|| {
            boot_result = Some(rt.block_on(boot(
                // No scripts queued: the dog's spawn is the first (and
                // only) one attempted, and finds nothing to pop.
                ScriptedRunner::new(vec![]),
                paths.clone(),
                BootOptions {
                    dogs: vec![DogSpec {
                        name: "metrics".to_string(),
                        source: DogSource::BuiltIn,
                    }],
                    ..BootOptions::default()
                },
            )));
        });
        let daemon = boot_result
            .unwrap()
            .expect("a dog that will not start must not fail the boot");

        let flock = rt
            .block_on(daemon.context().supervisor.list_checked())
            .unwrap();
        let dog = flock
            .iter()
            .find(|p| p.name == "metrics")
            .expect("the dog's entry must still be registered");
        assert_eq!(
            dog.status,
            ProcStatus::Errored,
            "a dog that could not spawn is errored, not silently absent"
        );
        assert!(
            logs.contains("metrics"),
            "the warning must name the dog that did not start: {logs:?}"
        );
        assert!(
            logs.contains("WARN"),
            "a dog failing to start is a warning, not silence: {logs:?}"
        );

        drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
    }

    /// `start_dog` is idempotent by name: enabling a dog under a name a sheep
    /// already holds comes back `Ok` over the sheep, not a started dog. The
    /// RPC arm inspects that reply for the missing `dog` marker, and this pins
    /// that `spawn_enabled_dogs` does the same.
    ///
    /// `#[test]` plus `capture_logs` for the reason
    /// `a_dog_that_will_not_start_does_not_fail_the_boot` gives.
    #[test]
    fn a_dog_enabled_under_a_sheeps_name_does_not_start_and_does_not_fail_the_boot() {
        let _guard = SIGNAL_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("metrics", "./srv"),
                instances_running: 1,
            }],
        };
        crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut boot_result = None;
        let logs = capture_logs(|| {
            boot_result = Some(rt.block_on(boot(
                // One script: the restored sheep's own spawn. `start_dog`
                // finds the name already registered and returns early
                // without ever touching the runner, so a second script
                // here would go unconsumed if that held.
                ScriptedRunner::new(vec![ProcScript::never_exits()]),
                paths.clone(),
                BootOptions {
                    restore: true,
                    dogs: vec![DogSpec {
                        name: "metrics".to_string(),
                        source: DogSource::BuiltIn,
                    }],
                    ..BootOptions::default()
                },
            )));
        });
        let daemon = boot_result
            .unwrap()
            .expect("a name collision must not fail the boot");

        let flock = rt
            .block_on(daemon.context().supervisor.list_checked())
            .unwrap();
        assert_eq!(
            flock.len(),
            1,
            "the collision must not register a second entry: {flock:?}"
        );
        assert!(
            flock[0].dog.is_none(),
            "the sheep must not be relabeled as a dog by a same-named enable: {:?}",
            flock[0]
        );
        assert!(
            logs.contains("metrics"),
            "the warning must name the collision: {logs:?}"
        );

        drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
    }

    /// fails if a promoted dog takes a saved sheep's name in silence. The
    /// unpromoted case above is the opposite way round: there the restore has
    /// already registered the sheep and `start_dog` returns over it, and here
    /// the dog registers against an empty flock and the sheep is what is
    /// lost. Nothing refuses either collision, so a warning is all the
    /// operator gets.
    ///
    /// `#[test]` plus `capture_logs` for the reason
    /// `a_dog_that_will_not_start_does_not_fail_the_boot` gives.
    #[test]
    fn a_promoted_dog_that_takes_a_saved_sheeps_name_says_so() {
        let _guard = SIGNAL_TEST_LOCK.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("metrics", "./srv"),
                instances_running: 1,
            }],
        };
        crate::snapshot::write_atomic(&paths.snapshot, &roll).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let mut boot_result = None;
        let logs = capture_logs(|| {
            boot_result = Some(rt.block_on(boot(
                // One script, and the dog is what consumes it: the restore
                // reads `metrics` as already running and starts nothing.
                ScriptedRunner::new(vec![ProcScript::never_exits()]),
                paths.clone(),
                BootOptions {
                    restore: true,
                    dogs: vec![DogSpec {
                        name: "metrics".to_string(),
                        source: DogSource::BuiltIn,
                    }],
                    boot_first_dogs: vec!["metrics".to_string()],
                    ..BootOptions::default()
                },
            )));
        });
        let daemon = boot_result
            .unwrap()
            .expect("a name collision must not fail the boot");

        let flock = rt
            .block_on(daemon.context().supervisor.list_checked())
            .unwrap();
        assert_eq!(flock.len(), 1, "one name is one entry: {flock:?}");
        assert!(
            flock[0].dog.is_some(),
            "the promoted dog is what holds the name: {:?}",
            flock[0]
        );
        assert!(
            logs.contains("is not restored"),
            "the lost sheep must be named out loud: {logs:?}"
        );

        drop(daemon); // no run() needed; SignalTasks::drop stops the listeners
    }

    /// The ordering `Type=notify` was chosen for: a unit that goes green at
    /// exec time reports a flock that is not up yet, and a hung restore reads
    /// as a healthy service supervising nothing.
    ///
    /// The restore announces its own spawn on the same socket, so what is
    /// asserted is the queue order of two datagrams. Reading only `READY=1`
    /// after `boot` returns would pass on a notify moved to the top of `boot`,
    /// since the kernel keeps that datagram queued however early it was sent.
    #[tokio::test]
    async fn readiness_is_reported_only_once_the_roll_is_restored() {
        // Real time: binds a real socket, so this obeys SIGNAL_TEST_LOCK's
        // rule like every other successful `boot()` in this module.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        init_dirs(&paths).unwrap();
        crate::snapshot::write_atomic(
            &paths.snapshot,
            &FlockSnapshot {
                version: SNAPSHOT_VERSION,
                saved_at_ms: 0,
                apps: vec![SavedApp {
                    app: AppConfig::minimal("web", "./srv"),
                    instances_running: 1,
                }],
            },
        )
        .unwrap();

        // Inside the TempDir and short: macOS caps a unix socket path near
        // 97 characters, which `test_paths` already keeps this under.
        let notify_path = dir.path().join("n.sock");
        let listener = std::os::unix::net::UnixDatagram::bind(&notify_path).unwrap();
        // Bounded: a datagram that never arrives must fail this case, not
        // park it.
        listener
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        // The marker sent from inside the restore's own spawn. AF_UNIX
        // SOCK_DGRAM enqueues synchronously and the two sends are sequential,
        // so the queue order is the program order.
        let runner = AnnouncingRunner::new(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            &notify_path,
        );

        let daemon = boot(
            runner,
            paths.clone(),
            BootOptions {
                restore: true,
                notify_socket: Some(notify_path.clone().into_os_string()),
                ..BootOptions::default()
            },
        )
        .await
        .unwrap();

        let mut buf = [0u8; 64];
        let read = listener.recv(&mut buf).unwrap();
        assert_eq!(
            &buf[..read],
            b"SPAWNED\n",
            "READY=1 arrived before the roll was restored: a unit that goes \
             green at exec time reports a flock that is not up yet, and a \
             restore that hangs reads as a healthy service supervising nothing"
        );
        let read = listener.recv(&mut buf).unwrap();
        assert_eq!(&buf[..read], b"READY=1\n");

        let ctx = daemon.context();
        let flock = ctx.supervisor.list_checked().await.unwrap();
        assert_eq!(flock.len(), 1, "the roll was actually restored");
        assert_eq!(flock[0].name, "web");

        ctx.shutdown();
        daemon.run().await.unwrap();
    }

    /// Nothing is bound at the address, so the send errors and the boot must
    /// still succeed: what failed is the init system's knowledge of a daemon
    /// that is otherwise up, which systemd reports through its own
    /// `TimeoutStartSec`.
    #[tokio::test]
    async fn a_readiness_datagram_that_cannot_be_delivered_does_not_fail_the_boot() {
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        let daemon = boot(
            ScriptedRunner::new(vec![]),
            paths.clone(),
            BootOptions {
                // Bound by nothing, and never created by anything: the send
                // is an error, which is the whole premise of the case.
                notify_socket: Some(dir.path().join("nobody.sock").into_os_string()),
                ..BootOptions::default()
            },
        )
        .await
        .expect("a daemon nobody could be told about is still a daemon");

        // Up enough to serve, not merely constructed.
        assert!(daemon.context().supervisor.list_checked().await.is_ok());

        daemon.context().shutdown();
        daemon.run().await.unwrap();
    }

    // `boot` is the one place `DEFAULT_MAX_CRON_SLEEP` is applied: the CLI
    // keeps the knob an `Option` all the way down, so nothing else here would
    // notice a different fallback. Whole `BootOptions` values rather than bare
    // `Option`s, since that is what `boot` reads.
    #[test]
    fn an_unset_max_cron_sleep_falls_back_to_the_daemons_own_default() {
        assert_eq!(
            max_cron_sleep(&BootOptions::default()),
            DEFAULT_MAX_CRON_SLEEP,
            "unset means the default"
        );
        assert_eq!(
            max_cron_sleep(&BootOptions {
                max_cron_sleep: Some(Duration::from_secs(300)),
                ..BootOptions::default()
            }),
            Duration::from_secs(300),
            "a configured value must reach the workers unchanged"
        );
    }

    // The only case driving `boot`'s own spawn of the extras reporter, over
    // the whole production chain: the actor arms the liveness loop at Online,
    // the loop reports over `Extras::real`'s sender, the reporter reads it,
    // and `extra_restart` lets it through. Real time, and a real `OsProber`.
    #[tokio::test]
    async fn a_booted_daemon_restarts_a_sheep_whose_liveness_probe_fails() {
        // Real time: binds a real socket, so it takes SIGNAL_TEST_LOCK.
        let _guard = SIGNAL_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);

        // Reserve a port, then release it: nothing listens there, so every
        // probe fails with a connection refusal and there is no race.
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reserved.local_addr().unwrap();
        drop(reserved);

        let daemon = boot(
            ScriptedRunner::new(vec![ProcScript::never_exits(); 4]),
            paths.clone(),
            BootOptions::default(),
        )
        .await
        .unwrap();
        let ctx = daemon.context();
        let mut events = ctx.events.subscribe();
        let run = tokio::spawn(daemon.run());

        let mut app = AppConfig::minimal("web", "./srv");
        app.liveness_probe = Some(ProbeConfig {
            kind: ProbeKind::Tcp,
            target: addr.to_string(),
            // The loop floors anything shorter at one second, so a smaller
            // number here would be a lie about what this test waits for.
            interval: UpDuration::from_millis(1_000),
            timeout: UpDuration::from_millis(500),
            failure_threshold: 1,
        });
        ctx.supervisor
            .start(vec![normalize(app).unwrap()])
            .await
            .unwrap();

        let restarted = async {
            loop {
                match events.recv().await.map(|event| event.to_event()) {
                    Ok(BusEvent::Process {
                        event: ProcessEventKind::Restart,
                        info,
                        ..
                    }) => return info,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("the event stream closed before a restart: {err}"),
                }
            }
        };
        let info = tokio::time::timeout(Duration::from_secs(20), restarted)
            .await
            .expect("a failing liveness probe must restart its sheep");
        assert_eq!(info.id, 0);
        assert_eq!(info.restarts, 1);

        ctx.shutdown();
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
