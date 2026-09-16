//! Lifecycle extras: what is armed when a sheep goes online, and what stops
//! when it goes terminal.
//!
//! Four subsystems run as free tasks beside the supervisor actor: the cron
//! worker, the memory-limit enforcer, the liveness prober and the filesystem
//! watch. [`ExtrasRegistry`] keys cron and watch per name, since both restart a
//! whole name-group, and the enforcer and the liveness loop per id.
//!
//! No trigger filters by status, so disarming is the whole of what keeps a
//! stopped sheep down. A group is torn down only when its last member leaves.

use core::fmt;
use core::time::Duration;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use shep_core::config::{AppConfig, CronSchedule, ProbeTarget};
use shep_core::values::UpDuration;

use crate::cron::{Clock, SystemClock, spawn_cron_worker};
use crate::entry::ProcessEntry;
use crate::limits::sample::{MemorySampler, SysinfoSampler};
use crate::limits::stats::StatsState;
use crate::limits::{LimitBreach, LimitEnforcer, PollingEnforcer};
use crate::probes::{LivenessFailure, Prober, spawn_liveness_task};
use crate::supervisor::SupervisorHandle;
use crate::watch::{
    DEFAULT_WATCH_DELAY, MIN_WATCH_DELAY, WatchFilter, own_log_ignores, spawn_watch_group,
};

/// A [`LivenessFailure`] paired with the epoch its probe was armed under.
///
/// `InstanceExtras::disarm` aborts without awaiting, so a probe already inside
/// `failures.send(..).await` can deliver after its replacement is running
/// against the same pid and status. The epoch tells the stale failure apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LivenessReport {
    /// The sheep's id.
    pub id: u32,
    /// The pid this loop was armed against.
    pub pid: u32,
    /// The epoch the reporting probe was armed under.
    pub epoch: u64,
}

/// Where the lifecycle extras send the two out-of-band failure reports.
///
/// The matching receivers belong to the reporting task, never to the actor.
#[derive(Debug, Clone)]
pub struct ExtrasReports {
    /// Memory-limit breaches, from the enforcer.
    pub breaches: mpsc::Sender<LimitBreach>,
    /// Sheep whose liveness probe hit `failure_threshold`.
    pub liveness: mpsc::Sender<LivenessReport>,
}

/// The four lifecycle extras, the seams they run on, and where their two
/// failure reports go.
///
/// Constructed once at boot and handed to the supervisor. Every seam is a
/// trait object so the engine's type does not grow a parameter per subsystem.
pub struct Extras {
    /// Wall clock the cron workers read.
    pub clock: Arc<dyn Clock>,
    /// Memory-limit mechanism.
    ///
    /// Shared so [`ExtrasRegistry::disarm`], whose signature takes no
    /// [`Extras`], can reach it too.
    pub enforcer: Arc<dyn LimitEnforcer>,
    /// Longest a cron worker parks before re-reading the clock, from
    /// `[daemon] max_cron_sleep`. Already defaulted: a value, not an option.
    pub max_cron_sleep: Duration,
    /// Cloned once per arming. The enforcer already holds its own breach
    /// sender; the liveness loops are free tasks and do not.
    pub reports: ExtrasReports,
    /// Live resource readings, shared with the RPC layer so a listing can take
    /// one on demand.
    pub stats: Arc<StatsState>,
}

impl Extras {
    /// The production wiring: system clock and polling enforcer over sysinfo.
    ///
    /// No prober: one is scoped to a single sheep's assembled environment.
    ///
    /// Must be called from within a Tokio runtime context: constructing the
    /// polling enforcer starts its sampling task immediately.
    #[must_use]
    pub fn real(reports: ExtrasReports, max_cron_sleep: Duration) -> Self {
        // One sampler behind both consumers: sampling and enforcement read the
        // same process table on the same tick, so a second `SysinfoSampler`
        // would mean a second syscall walk.
        let sampler: Arc<dyn MemorySampler> = Arc::new(SysinfoSampler::new());
        let stats = Arc::new(StatsState::new(Arc::clone(&sampler)));
        let enforcer =
            PollingEnforcer::start(sampler, reports.breaches.clone(), Arc::clone(&stats));
        Self {
            clock: Arc::new(SystemClock),
            enforcer: Arc::new(enforcer),
            max_cron_sleep,
            reports,
            stats,
        }
    }
}

impl fmt::Debug for Extras {
    // Roles, not values: neither seam is Debug.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Extras")
            .field("clock", &"<dyn Clock>")
            .field("enforcer", &"<dyn LimitEnforcer>")
            .field("max_cron_sleep", &self.max_cron_sleep)
            .finish_non_exhaustive()
    }
}

/// Restarts each sheep reported over `breaches` or `liveness`.
///
/// Ends when both senders have dropped. Owns both receivers: the actor must
/// never block on anything a subsystem controls.
///
/// Restarts go through [`SupervisorHandle::extra_restart`], never `restart`: a
/// report queued before `shep stop` is delivered after the sheep is `Stopped`,
/// and `restart` would resurrect it.
///
/// Must be called from within a Tokio runtime context: it spawns the reporting
/// task immediately.
pub fn spawn_extras_reporter(
    mut breaches: mpsc::Receiver<LimitBreach>,
    mut liveness: mpsc::Receiver<LivenessReport>,
    supervisor: SupervisorHandle,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // A closed `mpsc::Receiver` resolves to `None` on every poll, so a
        // branch left in consideration would busy-spin the loop.
        let mut breaches_open = true;
        let mut liveness_open = true;
        while breaches_open || liveness_open {
            tokio::select! {
                maybe_breach = breaches.recv(), if breaches_open => match maybe_breach {
                    Some(breach) => {
                        tracing::warn!(
                            id = breach.id,
                            pid = breach.root_pid,
                            observed = %breach.observed,
                            limit = %breach.limit,
                            "process tree exceeded its max_memory; restarting"
                        );
                        // No epoch: a breach has no probe task to abort
                        // mid-`send`. It carries the observed size, and the
                        // actor re-checks it against the ceiling in force.
                        supervisor
                            .extra_restart(
                                breach.id,
                                breach.root_pid,
                                None,
                                Some(breach.observed),
                            )
                            .await;
                    }
                    None => breaches_open = false,
                },
                maybe_failure = liveness.recv(), if liveness_open => match maybe_failure {
                    Some(report) => {
                        tracing::warn!(
                            id = report.id,
                            pid = report.pid,
                            "liveness probe hit its failure_threshold; restarting"
                        );
                        supervisor
                            .extra_restart(report.id, report.pid, Some(report.epoch), None)
                            .await;
                    }
                    None => liveness_open = false,
                },
            }
        }
    })
}

/// Per-sheep and per-group task handles, armed on `online` and aborted on the
/// way out.
#[derive(Debug, Default)]
pub struct ExtrasRegistry {
    /// One name-group's per-name tasks. Keyed on the configuration, not on
    /// what an arming managed to build.
    groups: HashMap<String, NameExtras>,
    /// One instance's per-pid extras, keyed by sheep id. Present only while
    /// at least one of them is armed.
    instances: HashMap<u32, InstanceExtras>,
    /// The epoch each id's liveness probe is currently armed under, bumped by
    /// [`Self::arm`] whether or not that id configures a `liveness_probe`, so
    /// an app that adds one later inherits no stale count.
    ///
    /// Separate from the supervisor's `SheepSlot::epoch`, which moves on a
    /// respawn: this one answers whether a probe was replaced without the
    /// process underneath it changing.
    liveness_epochs: HashMap<u32, u64>,
}

/// One name-group's per-name tasks, plus the armed instances keeping them
/// alive.
///
/// Either task may be `None` while the group still exists: an app whose watch
/// could not be registered is a member of its group all the same.
#[derive(Debug, Default)]
struct NameExtras {
    /// The group's cron worker, when the app configures `cron_restart`.
    cron: Option<JoinHandle<()>>,
    /// The group's filesystem watch, when the app configures `watch`.
    watch: Option<JoinHandle<()>>,
    /// Ids of this name's instances that currently have anything armed. The
    /// last one leaving is what tears the two tasks above down.
    members: HashSet<u32>,
}

impl NameExtras {
    /// Aborts both per-name tasks. Takes `self`: a group is torn down once.
    fn abort(self) {
        if let Some(cron) = self.cron {
            cron.abort();
        }
        if let Some(watch) = self.watch {
            watch.abort();
        }
    }
}

/// One instance's per-pid extras.
struct InstanceExtras {
    /// Where this id's sampling was started. Not an `Option`: every sheep
    /// with a pid is sampled.
    stats: Arc<StatsState>,
    /// The enforcer this id's memory limit was armed against.
    limit: Option<Arc<dyn LimitEnforcer>>,
    /// The liveness loop, when the app configures `liveness_probe`.
    liveness: Option<JoinHandle<()>>,
}

impl InstanceExtras {
    /// Sampling armed and nothing else: what an app configuring neither
    /// `max_memory` nor `liveness_probe` gets.
    fn watched_only(stats: Arc<StatsState>) -> Self {
        Self {
            stats,
            limit: None,
            liveness: None,
        }
    }

    /// Undoes this instance's arming: sampling, the memory limit against `id`,
    /// and the liveness loop.
    fn disarm(self, id: u32) {
        self.stats.unwatch(id);
        if let Some(enforcer) = self.limit {
            enforcer.disarm(id);
        }
        if let Some(liveness) = self.liveness {
            liveness.abort();
        }
    }
}

impl fmt::Debug for InstanceExtras {
    // `Arc<dyn LimitEnforcer>` is not Debug, and the useful fact is that an
    // arming exists. `stats` is armed for every instance, hence the `..`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstanceExtras")
            .field("limit_armed", &self.limit.is_some())
            .field("liveness", &self.liveness)
            .finish_non_exhaustive()
    }
}

impl ExtrasRegistry {
    /// Arms everything an entry's configuration asks for.
    ///
    /// `prober` is scoped to this instance's assembled `SpawnSpec` and is read
    /// only by the liveness loop.
    ///
    /// Idempotent per id: arming an already-armed id disarms that id's own
    /// per-pid extras first, which is what a respawn needs. A live name-group
    /// task is left alone: it is keyed on the name and outlives any one process.
    /// Rebuilding it re-registers the OS watcher, which can fail and would
    /// silently cost the app its watch.
    pub fn arm(
        &mut self,
        entry: &ProcessEntry,
        prober: Arc<dyn Prober>,
        extras: &Extras,
        supervisor: &SupervisorHandle,
    ) {
        let config = entry.spec.config();
        let id = entry.id;

        self.disarm_instance(id);
        // Bumped ahead of `arm_instance`, which reads it only for a probe it
        // is about to spawn.
        let liveness_epoch = self.liveness_epochs.entry(id).or_insert(0);
        *liveness_epoch += 1;
        let liveness_epoch = *liveness_epoch;
        if let Some(instance) = arm_instance(entry, prober, extras, liveness_epoch) {
            self.instances.insert(id, instance);
        }

        // Membership is decided by the configuration, never by whether this
        // arming built a task: a transient `arm_watch` failure would otherwise
        // leave a still-online instance out of its own group.
        if config.cron_restart.is_none() && !config.watch {
            return;
        }
        let group = self.groups.entry(config.name.clone()).or_default();
        // A second instance of a name joins the group rather than arming a
        // second worker: both triggers already reach every instance of the name.
        group.members.insert(id);
        // A task can end on its own: a cron worker returns on a pattern with no
        // further occurrence, and the watch loop returns when its `WatchSource`
        // dies. Presence in the map is therefore not the test.
        if group.cron.as_ref().is_none_or(JoinHandle::is_finished) {
            group.cron = arm_cron(config, extras, supervisor);
        }
        // An app whose watch can never arm pays a fresh `canonicalize`, globset
        // compile and `warn!` on every re-arm, bounded by `max_restarts`. That is
        // the price of retrying the transient failures this rebuild exists for.
        if group.watch.as_ref().is_none_or(JoinHandle::is_finished) {
            group.watch = arm_watch(entry, supervisor);
        }
    }

    /// Aborts everything armed for `id`, and both of the name-group's per-name
    /// tasks when this was the last armed instance of the name.
    ///
    /// No trigger filters by status, so a sheep stays down because nothing is
    /// left armed for it. Aborting the watch-group handle stops the OS watch:
    /// the debouncer guard rides inside the aborted future.
    pub fn disarm(&mut self, id: u32, name: &str) {
        self.disarm_instance(id);
        // Not inside `disarm_instance`: `Self::arm` calls that first and would
        // reset the counter it is about to bump.
        self.liveness_epochs.remove(&id);

        let Some(group) = self.groups.get_mut(name) else {
            return;
        };
        // An id that was never a member leaves the group untouched; a group
        // with instances left standing keeps its tasks.
        if !group.members.remove(&id) || !group.members.is_empty() {
            return;
        }
        if let Some(group) = self.groups.remove(name) {
            group.abort();
        }
    }

    /// Rebuilds everything armed for `name`, replacing live tasks rather than
    /// keeping them.
    ///
    /// The group-scoped fields (`watch`, `ignore_watch`, `watch_delay`,
    /// `watch_options`, `cron_restart`, `cron_timezone`) are read when the task
    /// is built, so a task [`Self::arm`] left alive would keep the old values.
    /// The rebuild costs a real gap in the OS watch with no rescan.
    ///
    /// `entries` is what the caller wants armed: a stopped instance passed here
    /// joins a group whose next cron occurrence or watch event restarts it, and
    /// an empty slice aborts the group and rebuilds nothing. `prober` runs once
    /// per entry, since `assemble` bakes `SHEP_INSTANCE` into its environment.
    pub fn rearm_name(
        &mut self,
        name: &str,
        entries: &[&ProcessEntry],
        prober: impl Fn(&ProcessEntry) -> Arc<dyn Prober>,
        extras: &Extras,
        supervisor: &SupervisorHandle,
    ) {
        // Removing the entry rather than mutating it makes the rebuild take
        // `arm`'s own "no task yet" path.
        if let Some(group) = self.groups.remove(name) {
            group.abort();
        }
        for entry in entries {
            self.arm(entry, prober(entry), extras, supervisor);
        }
    }

    /// Undoes one instance's per-pid arming. A no-op for an id with none.
    fn disarm_instance(&mut self, id: u32) {
        if let Some(instance) = self.instances.remove(&id) {
            instance.disarm(id);
        }
    }

    /// The epoch `id`'s liveness probe is currently armed under, or `0` for an
    /// id that has never been armed. `Actor::handle_extra_restart` drops a
    /// [`LivenessReport`] whose epoch does not match.
    pub(crate) fn liveness_epoch(&self, id: u32) -> u64 {
        self.liveness_epochs.get(&id).copied().unwrap_or(0)
    }

    /// The ids in `name`'s armed group, or `None` when nothing of that name is
    /// armed at all.
    #[cfg(test)]
    pub(crate) fn group_members(&self, name: &str) -> Option<Vec<u32>> {
        self.groups.get(name).map(|group| {
            let mut members: Vec<u32> = group.members.iter().copied().collect();
            members.sort_unstable();
            members
        })
    }
}

impl Drop for ExtrasRegistry {
    // Here rather than a disarm loop in `begin_shutdown`, which a
    // `WaitingRestart` sheep never reaches and a panicking actor never runs. A
    // dropped `JoinHandle` detaches its task rather than cancelling it, and while
    // any task lives it holds a report sender that keeps the reporter alive.
    fn drop(&mut self) {
        for (id, instance) in self.instances.drain() {
            instance.disarm(id);
        }
        for (_name, group) in self.groups.drain() {
            group.abort();
        }
    }
}

/// Arms the per-pid extras: sampling always, the memory limit and the liveness
/// loop where the app configures them. `None` when the entry has no pid.
fn arm_instance(
    entry: &ProcessEntry,
    prober: Arc<dyn Prober>,
    extras: &Extras,
    liveness_epoch: u64,
) -> Option<InstanceExtras> {
    let config = entry.spec.config();
    let wants_anything = config.max_memory.is_some() || config.liveness_probe.is_some();
    let Some(pid) = entry.pid else {
        if wants_anything {
            // Unreachable from the transition this is called at: a sheep is
            // Online only with a live pid. Both extras are armed against a pid.
            tracing::warn!(
                id = entry.id,
                "arming a sheep with no pid; its memory limit and liveness probe stay disarmed"
            );
        }
        return None;
    };

    // Unconditional, unlike the two below: a listing reports CPU and memory for
    // every sheep.
    extras.stats.watch(entry.id, pid);
    let mut instance = InstanceExtras::watched_only(Arc::clone(&extras.stats));
    if let Some(limit) = config.max_memory {
        extras.enforcer.arm(entry.id, pid, limit);
        instance.limit = Some(Arc::clone(&extras.enforcer));
    }
    if let Some(probe) = config.liveness_probe.as_ref() {
        match ProbeTarget::parse(probe) {
            Ok(target) => {
                // `probes` knows only `LivenessFailure`, so the probe reports
                // into a private channel and this relay tags its one failure
                // with the epoch it was spawned under. Captured here rather than
                // read at delivery, when a re-arm may have moved it on.
                let (raw_tx, mut raw_rx) = mpsc::channel::<LivenessFailure>(1);
                let reports_liveness = extras.reports.liveness.clone();
                tokio::spawn(async move {
                    if let Some(failure) = raw_rx.recv().await {
                        let _ = reports_liveness
                            .send(LivenessReport {
                                id: failure.id,
                                pid: failure.pid,
                                epoch: liveness_epoch,
                            })
                            .await;
                    }
                });
                instance.liveness = Some(spawn_liveness_task(
                    entry.id,
                    pid,
                    probe.clone(),
                    target,
                    prober,
                    raw_tx,
                ));
            }
            Err(err) => {
                // `normalize` already parses both probe targets, so a config
                // that reached the daemon cannot land here. Swallowed rather
                // than `expect`-ed: a future path skipping normalization costs
                // one app its probe rather than the daemon.
                tracing::warn!(
                    id = entry.id,
                    name = config.name.as_str(),
                    %err,
                    "liveness_probe target could not be parsed; arming no liveness probe"
                );
            }
        }
    }
    Some(instance)
}

/// Spawns the name-group's cron worker, or `None` when the app configures no
/// `cron_restart` or names a pattern that will not parse.
///
/// An unparseable pattern costs the app its schedule, and the `warn!` is the
/// only record: the app comes up `online` either way.
fn arm_cron(
    config: &AppConfig,
    extras: &Extras,
    supervisor: &SupervisorHandle,
) -> Option<JoinHandle<()>> {
    let pattern = config.cron_restart.as_ref()?;
    let schedule = match CronSchedule::parse(pattern, config.cron_timezone.as_deref()) {
        Ok(schedule) => schedule,
        Err(err) => {
            tracing::warn!(
                name = config.name.as_str(),
                pattern = pattern.as_str(),
                %err,
                "cron_restart pattern could not be parsed; arming no cron worker"
            );
            return None;
        }
    };
    Some(spawn_cron_worker(
        config.name.clone(),
        schedule,
        Arc::clone(&extras.clock),
        supervisor.clone(),
        extras.max_cron_sleep,
    ))
}

/// Spawns the name-group's filesystem watch, or `None` when the app does not
/// ask to be watched, or when its root or its globs will not resolve.
///
/// Every failure here arms no watch rather than propagating: a watch root that
/// will not resolve must not take down the same app's cron worker, enforcer and
/// probe. Each writes a `warn!`, and that record is the entire signal.
///
/// Takes the whole [`ProcessEntry`] because the assembled `out_file`/`err_file`
/// are what `own_log_ignores` needs.
fn arm_watch(entry: &ProcessEntry, supervisor: &SupervisorHandle) -> Option<JoinHandle<()>> {
    let config = entry.spec.config();
    if !config.watch {
        return None;
    }
    let Some(cwd) = config.cwd.as_deref() else {
        // `normalize` rejects `watch = true` with no `cwd`. The daemon's own
        // working directory is no fallback: a systemd unit would watch `/`.
        tracing::warn!(
            name = config.name.as_str(),
            "watch is on but the app names no cwd; arming no watch"
        );
        return None;
    };
    // Canonicalized, not merely absolute: the group loop strips this prefix off
    // the absolute paths notify delivers, and on macOS a directory under
    // `/var/...` arrives from FSEvents as `/private/var/...`.
    let root = match std::fs::canonicalize(cwd) {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!(
                name = config.name.as_str(),
                path = cwd,
                %err,
                "watch root could not be resolved; arming no watch"
            );
            return None;
        }
    };
    // The app's own ignores plus this sheep's log files, for whichever of them
    // the app pointed back inside the watched tree.
    let mut ignores = config.ignore_watch.clone();
    ignores.extend(own_log_ignores(
        &root,
        [entry.out_file.as_path(), entry.err_file.as_path()],
    ));
    let filter = match WatchFilter::new(&config.watch_options, &ignores) {
        Ok(filter) => filter,
        Err(err) => {
            // `normalize` compiles every `watch_options` and `ignore_watch`
            // pattern, so a config that reached the daemon cannot land here.
            tracing::warn!(
                name = config.name.as_str(),
                %err,
                "watch globs could not be compiled; arming no watch"
            );
            return None;
        }
    };
    match spawn_watch_group(
        config.name.clone(),
        root,
        filter,
        watch_delay_for(config),
        supervisor.clone(),
    ) {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!(
                name = config.name.as_str(),
                %err,
                "the OS watch could not be started; arming no watch"
            );
            None
        }
    }
}

/// The debounce window an app's watch is armed with: its own `watch_delay` when
/// it set one, [`DEFAULT_WATCH_DELAY`] otherwise, floored at [`MIN_WATCH_DELAY`].
///
/// The floor is a last line of defence: `normalize` already refuses
/// `watch_delay = "0"`.
fn watch_delay_for(config: &AppConfig) -> Duration {
    config
        .watch_delay
        .map(UpDuration::as_duration)
        .unwrap_or(DEFAULT_WATCH_DELAY)
        .max(MIN_WATCH_DELAY)
}

#[cfg(test)]
mod tests;
