//! Daemon boot: the sequence, and a file for each step of it
//!
//! [`boot`] is the order, and the order is the point. Signal handlers before
//! anything observable exists, the layout and then the pidfile claim before
//! the socket bind they make race-free, the muster restore between the two
//! halves of the dog spawn, and readiness last. `layout.rs`, `pidfile.rs`,
//! `socket.rs` and `signals.rs` hold those steps, `handover.rs` holds both
//! ends of `shep daemon reload`, and `running.rs` holds what `boot` hands
//! back: [`RunningDaemon::run`] serves until a signal or `KillDaemon`, then
//! tears down in a load-bearing order.
//!
//! [`BootOptions::ready_fd`] arrives as an owned [`std::fs::File`]:
//! `crate::sys::adopt_fd`'s ordering precondition is process-wide and `boot`
//! is `async`, so only the CLI's `main` can discharge it.

mod error;
#[cfg(unix)]
mod handover;
mod layout;
mod pidfile;
mod running;
mod signals;
mod socket;
#[cfg(all(test, unix))]
mod tests;

pub use self::error::BootError;
pub use self::layout::DIR_MODE;
pub(crate) use self::layout::init_dirs;
pub use self::pidfile::{Shepherd, daemon_liveness, pidfile};
pub use self::running::RunningDaemon;
pub use self::socket::READY_FD_ENV;

use core::time::Duration;
use std::ffi::{OsStr, OsString};
#[cfg(all(test, windows))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use shep_core::paths::ShepPaths;

#[cfg(unix)]
use self::handover::{HandoverSeam, Rehydrated, apps_for_the_roll, rehydrate, successor_handover};
use self::pidfile::PidfileLock;
use self::signals::install_signals;
use self::socket::{DaemonReady, bind_socket, socket_path, write_ready};
use crate::bus::{Bus, new_bus};
use crate::cron::DEFAULT_MAX_CRON_SLEEP;
use crate::dogs::{DogSpec, spawn_dog_watch};
use crate::extras::{Extras, ExtrasReports, spawn_extras_reporter};
use crate::rpc::RpcContext;
use crate::runner::ProcessRunner;
use crate::snapshot::{self, FlockRegistry, spawn_snapshot_writer};
use crate::supervisor::{SupervisorBuilder, SupervisorHandle};
use crate::sweep::LambSweep;

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
    /// Every dog this shepherd may hold a section for, running or not, with
    /// the source it would spawn from: the built-in dogs, every dog
    /// `[daemon] adopted_dogs` records, and whatever `[daemon] enabled_dogs`
    /// names.
    ///
    /// Assembled by the caller from the same file [`Self::dogs`] comes out
    /// of, and for the same reason: shep-daemon never reads `shep.toml`
    /// itself. A [`shep_core::protocol::DogSource::Adopted`] carries whether
    /// the dog declared the shepherd channel (`[daemon] channel_dogs`), which
    /// is what lets a dog be registered at rest without the file in hand.
    ///
    /// A superset of [`Self::dogs`], and the difference is the whole point
    /// of carrying both. That one is the spawn list, so it holds only the
    /// dogs an operator has switched on; this one holds the dogs that
    /// exist. Every entry here is registered once the flock is restored: a
    /// dog outside [`Self::dogs`] sits `Stopped` and disabled until an
    /// `EnableDog` starts it. `Request::SetDogConfig` is guarded on this
    /// one, because the dog most in need of configuring is the one that is
    /// disabled or has never started, and a guard on the running set
    /// refuses exactly that dog.
    ///
    /// A dog in [`Self::dogs`] and absent here is registered all the same.
    pub known_dogs: Vec<DogSpec>,
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
    options: BootOptions,
) -> Result<RunningDaemon, BootError> {
    boot_with_sweep(runner, paths, options, None).await
}

/// [`boot`], ending lambs through `sweep` in place of the real one when given.
///
/// # Errors
///
/// As [`boot`].
pub(crate) async fn boot_with_sweep<R: ProcessRunner>(
    runner: R,
    paths: ShepPaths,
    mut options: BootOptions,
    sweep: Option<Arc<dyn LambSweep>>,
) -> Result<RunningDaemon, BootError> {
    // Before anything else: it reads the current directory, which is the
    // startup directory only until something moves it.
    #[cfg(unix)]
    crate::handover::record_launch_path();

    let delete_flock_on_shutdown = options.delete_flock_on_shutdown;

    // 1. Signal handlers, before the socket or anything else observable
    //    exists.
    let (shutdown, shutdown_rx) = watch::channel(false);
    #[cfg(unix)]
    let (signals, connect_supervisor, connect_handover) =
        install_signals(shutdown.clone(), paths.clone())?;
    #[cfg(windows)]
    let (signals, connect_supervisor) = install_signals(shutdown.clone(), paths.clone())?;

    // 2. Layout, then claim $SHEP_HOME before touching the socket: that is
    //    what closes the concurrent-boot race a bare probe-then-recover
    //    sequence cannot. Held for the rest of this daemon's life.
    init_dirs(&paths)?;
    let socket = socket_path(&paths, options.socket.as_deref());
    // A successor takes neither the lock nor the address: it inherited both,
    // still held. Rebinding would race the predecessor's own socket file, and
    // re-locking would mean releasing first.
    #[cfg(unix)]
    let (mut pidfile_lock, listener, inherited) = match successor_handover()? {
        Some(carried) => {
            let Rehydrated {
                lock,
                listener,
                flock,
                counters,
                reloads,
            } = rehydrate(carried)?;
            (lock, listener, Some((flock, counters, reloads)))
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
    let mut extras = Extras::real(
        ExtrasReports {
            breaches: breach_tx,
            liveness: live_tx,
        },
        max_cron_sleep(&options),
    );
    if let Some(sweep) = sweep {
        extras.lamb_sweep = sweep;
    }
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
    // Taken before the runner moves into the supervisor. `None` for a runner
    // that opens no dog connections.
    let dog_links = runner.dog_links();
    // One table, two owners: the server tasks enter every connection they
    // serve and the SIGHUP task reads them out to carry across the exec.
    #[cfg(unix)]
    let dog_link_table = crate::server::DogLinks::default();
    let mut builder = SupervisorBuilder::new(runner, paths.clone(), events.clone())
        .extras(extras)
        .provider_secrets(Arc::clone(&provider_secrets));
    // Cloned before the `take` below feeds the supervisor, and defaulted the
    // way the builder defaults it, so a dog and a sheep that name no
    // environment resolve in the same one.
    let host_environment = options
        .environment
        .clone()
        .unwrap_or_else(|| crate::supervisor::DEFAULT_ENVIRONMENT.to_string());
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
                .map_err(|source| BootError::Adopt(Box::new(source)))?
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
        dog_links: dog_link_table.clone(),
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

    // A saved sheep keeps its name against a dog, built-in or adopted: an
    // upgrade that starts listing the built-ins must not cost anyone an app.
    if options.restore && !inherited_flock {
        crate::dogs::leave_held_names(
            &mut options.dogs,
            &mut options.known_dogs,
            &crate::dogs::saved_sheep_names(&paths.snapshot),
        );
    }

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

    // Starts empty and is not carried across a handover: a successor has
    // refused nobody.
    let dog_refusals = crate::dogs::DogRefusals::new();
    // Also not carried, so a successor must not claim a pid it has never seen
    // never called. `PEER_CONTACT_WARMUP` is what makes starting empty safe:
    // until the map has listened long enough for an absence to mean
    // something it answers `Contact::Unknown` rather than `Contact::None`.
    let peer_contacts = crate::dogs::PeerContacts::new();

    // Built ahead of the first dog, which is spawned holding a connection to
    // this shepherd: the context is what answers it, and the control socket's
    // listener is not served until the restore below has finished.
    let ctx = RpcContext {
        supervisor: supervisor.clone(),
        events: events.clone(),
        registry: registry.clone(),
        snapshot_path: paths.snapshot.clone(),
        dogs_config: paths.dogs_config.clone(),
        known_dogs: known_dog_grants(&options),
        dog_names: dog_names.clone(),
        boot_first_dogs: options.boot_first_dogs.clone(),
        paths: paths.clone(),
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        dog_refusals: dog_refusals.clone(),
        peer_contacts: peer_contacts.clone(),
        pid,
        shutdown: shutdown.clone(),
        stats,
        // Starts its own tick here rather than riding the supervisor's
        // extras: those are armed and disarmed per sheep, and this reads
        // the machine whether the flock is empty or not.
        host: crate::host::HostState::real(),
        provider_secrets,
        host_environment: host_environment.clone(),
    };
    if let Some(links) = dog_links {
        tokio::spawn(crate::server::serve_dog_links(
            links,
            ctx.clone(),
            #[cfg(unix)]
            dog_link_table,
            shutdown.subscribe(),
        ));
    }

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
            options.notify_socket.as_deref(),
        )
        .await?;
        // The last stage's extension may be spent by now, and the setup below
        // still runs before `READY=1`.
        crate::boot_order::extend_start_deadline(
            options.notify_socket.as_deref(),
            crate::boot_order::STAGE_SLACK,
        );
    }

    // After the restore, so no saved sheep's id moves, and before the enabled
    // dogs below, so each of those finds its entry at rest.
    crate::dogs::register_dogs_at_rest(&options.known_dogs, &options.dogs, &paths, &supervisor)
        .await;

    crate::dogs::spawn_enabled_dogs(&rest, &paths, &supervisor, &events).await;

    // Spawned at every boot, a successor's included, rather than anchored to a
    // dog's own spawn. It restarts a dog that has been running without ever
    // answering this shepherd; the tradeoff is argued at `record_silent_dog`.
    let silent_dog_watch = crate::dogs::spawn_silent_dog_watch(
        supervisor.clone(),
        dog_refusals.clone(),
        peer_contacts.clone(),
        events.clone(),
    );

    // Ends on `shutdown`, so teardown holds no handle for it. A successor
    // starts its own and sets its own baseline, so a handover wakes no dog.
    drop(crate::secrets_watch::spawn_secrets_watch(
        crate::secrets_watch::Watcher::new(
            paths.secrets.clone(),
            paths.dogs_config.clone(),
            host_environment.clone(),
        ),
        events.clone(),
        shutdown.subscribe(),
    ));

    let writer = spawn_snapshot_writer(
        paths.snapshot.clone(),
        supervisor.clone(),
        registry.clone(),
        events.subscribe(),
    );

    // 5. A failure is a `warn!` and the boot continues: only systemd's view
    //    is wrong. Unix only, since `$NOTIFY_SOCKET` is a unix datagram
    //    socket; the field stays on `BootOptions` for both platforms.
    //
    //    Reported here, after the restore, which is the honest answer to
    //    `Type=notify`. The restore holds each stage until its members are
    //    ready, so it can outlast `TimeoutStartSec`; `restore_flock` has each
    //    stage extend that deadline by its own bound, before its spawns and
    //    again before its wait, and the restore extends it once more for the
    //    setup that follows. The unit is killed only by a stage that overruns
    //    its own bound.
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

/// Every dog `options` makes known, with the grants its source carries: the
/// known list plus the enabled one.
fn known_dog_grants(options: &BootOptions) -> crate::rpc::KnownDogs {
    crate::rpc::KnownDogs::with_grants(
        options
            .known_dogs
            .iter()
            .chain(&options.dogs)
            .map(|spec| (spec.name.clone(), spec.source.grants().cloned())),
    )
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
    notify_socket: Option<&OsStr>,
) -> Result<(), BootError> {
    snapshot::muster(
        &paths.snapshot,
        registry,
        supervisor,
        events,
        dogs,
        boot_first_dogs,
        notify_socket,
    )
    .await?;
    Ok(())
}

/// [`boot`] over a sweep that finds no lambs.
///
/// Every boot test here drives a `ScriptedRunner`, whose pids can name real
/// processes on the host. The real sweep would end their children.
#[cfg(all(test, unix))]
pub(crate) async fn boot_with_idle_sweep<R: ProcessRunner>(
    runner: R,
    paths: ShepPaths,
    options: BootOptions,
) -> Result<RunningDaemon, BootError> {
    boot_with_sweep(runner, paths, options, Some(crate::fake::idle_sweep())).await
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
