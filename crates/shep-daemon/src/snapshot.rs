//! The muster roll: persisted flock state for restart-survival (`shep muster`)
//!
//! `FlockRegistry::roll` turns the registry's [`AppConfig`]s plus a live
//! [`ProcessInfo`] listing into a [`FlockSnapshot`], written to `flock.json`
//! by `write_atomic`. A `SnapshotWriter` task debounces lifecycle events so a
//! restart storm produces one write. `restorable` re-validates every entry,
//! since the file is human-editable.
//!
//! `muster` reads the roll and starts those apps, from `boot` and from the
//! `Muster` request alike, stage by stage in `depends_on` order
//! (`crate::boot_order`). An app restores iff it was running when the roll was
//! saved (`instances_running > 0`) and `autostart` is still true; one the flock
//! already has is left where it stands, still reported as restored. Nothing
//! about the graph refuses a restore: a cycle, a name nothing answers to and a
//! dependency that opted out of `autostart` are each warned about and started
//! around, because the machine this runs on has nobody watching it.

use core::fmt;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::sync::Notify;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep_until};

use shep_core::config::graph::{BootPlan, render_cycle};
use shep_core::config::{AppConfig, NormalizeError, ResolvedApp, normalize};
use shep_core::protocol::{BusEvent, ProcessInfo};
use shep_core::status::ProcStatus;

use crate::bus::{Bus, SharedEvent};
use crate::supervisor::{BatchPolicy, SupervisorHandle};

/// Schema version of `flock.json`
pub(crate) const SNAPSHOT_VERSION: u32 = 1;

/// How long the writer lets a burst of lifecycle events settle before it
/// rewrites the roll.
///
/// One restart emits Exit + Restart + Start + Online within microseconds;
/// 250 ms folds a whole restart storm into a single atomic write while still
/// landing the roll orders of magnitude faster than the reboot it protects
/// against (spec §13.4).
pub(crate) const SNAPSHOT_DEBOUNCE_MS: u64 = 250;

/// The muster roll: which apps were registered, and how many were up
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlockSnapshot {
    /// Schema version this roll was written under (`SNAPSHOT_VERSION`)
    pub version: u32,
    /// Wall-clock milliseconds since the Unix epoch when this roll was built
    pub saved_at_ms: u64,
    /// One entry per sheep still known to the flock at save time
    pub apps: Vec<SavedApp>,
}

/// One sheep's entry in a [`FlockSnapshot`]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedApp {
    /// The config the sheep was started from (Debug redacts `env`, through
    /// [`AppConfig`]'s own redacting `Debug`)
    pub app: AppConfig,
    /// How many instances of this sheep were running when the roll was built
    pub instances_running: u32,
}

/// The daemon's record of the config each registered sheep was started from
///
/// The supervisor owns runtime state; nothing in a [`ProcessInfo`] can
/// reproduce the `AppConfig` a sheep came from, which is exactly what a roll
/// needs. Cheap to clone (one `Arc`).
#[derive(Debug, Clone, Default)]
pub(crate) struct FlockRegistry {
    apps: Arc<Mutex<BTreeMap<String, AppConfig>>>,
    /// Woken by every write to `apps`, so the roll writer schedules a file
    /// write for a change that moves no process and so publishes no
    /// [`BusEvent::Process`] of its own.
    dirty: Arc<Notify>,
}

impl FlockRegistry {
    /// Builds an empty registry
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records (or re-records) each app's config, keyed by name.
    pub(crate) fn record(&self, apps: &[ResolvedApp]) {
        let mut map = self.apps.lock().unwrap_or_else(PoisonError::into_inner);
        for app in apps {
            map.insert(app.config().name.clone(), app.config().clone());
        }
        drop(map);
        self.dirty.notify_one();
    }

    /// Records one app's config directly, for a successor rebuilding this
    /// registry from a handover blob.
    ///
    /// [`Self::record`] takes [`ResolvedApp`]s because every other caller has
    /// just normalized one. A successor holds only the [`AppConfig`] the blob
    /// carried, and the supervisor re-normalizes that on its own way to
    /// installing the sheep.
    ///
    /// The roll is written from this registry, so a successor that left it
    /// empty would overwrite a good roll within seconds of taking over.
    #[cfg(unix)]
    pub(crate) fn record_config(&self, config: &AppConfig) {
        self.apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(config.name.clone(), config.clone());
        self.dirty.notify_one();
    }

    /// Drops every recorded app, so the next [`Self::roll`] describes an
    /// empty flock regardless of what is still live in the supervisor's own
    /// listing.
    ///
    /// The one caller is [`crate::boot::RunningDaemon::run`]'s teardown, under
    /// `BootOptions::delete_flock_on_shutdown`: `shep dev`'s isolated session,
    /// where nothing should survive for a later `shep muster`. A production
    /// shutdown never calls it.
    pub(crate) fn clear(&self) {
        self.apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        self.dirty.notify_one();
    }

    /// Builds the roll from the live listing, pruning names the flock no
    /// longer has (a deleted sheep must not resurrect).
    #[must_use]
    pub(crate) fn roll(&self, infos: &[ProcessInfo], now_ms: u64) -> FlockSnapshot {
        // A poisoned lock recovers instead of panicking: the map is a plain
        // BTreeMap, so a panic elsewhere cannot leave it inconsistent, and
        // taking the daemon down over it would be the worse failure.
        let mut apps = self.apps.lock().unwrap_or_else(PoisonError::into_inner);
        apps.retain(|name, _| infos.iter().any(|info| &info.name == name));
        let saved = apps
            .iter()
            .map(|(name, app)| SavedApp {
                app: app.clone(),
                instances_running: u32::try_from(
                    infos
                        .iter()
                        .filter(|i| &i.name == name && is_running(i.status))
                        .count(),
                )
                .unwrap_or(u32::MAX),
            })
            .collect();
        FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: now_ms,
            apps: saved,
        }
    }
}

/// True for the statuses [`FlockRegistry::roll`] counts as "up".
///
/// [`ProcStatus::Stopping`] is absent: a reload's drainee and its replacement
/// hold the same instance slot, so counting both would report two running
/// instances for one.
fn is_running(status: ProcStatus) -> bool {
    matches!(
        status,
        ProcStatus::Online | ProcStatus::Starting | ProcStatus::WaitingRestart
    )
}

/// Error type returned from `write_atomic` and [`read`]
///
/// Wraps `io::Error`/`serde_json::Error` directly rather than stringifying
/// them, so callers keep the underlying diagnostic through
/// [`core::error::Error::source`]. That costs the enum
/// `Clone`/`PartialEq`/`Eq`.
///
/// `#[non_exhaustive]`: a future roll-format refusal would need its own
/// variant, distinct from [`Self::Parse`]'s catch-all.
#[non_exhaustive]
#[derive(Debug)]
pub enum SnapshotError {
    /// The roll path has no parent directory to create the temp file in
    /// (carries the path)
    NoParent(PathBuf),
    /// The roll failed to serialize to JSON
    Encode(serde_json::Error),
    /// The temp file, `fsync`, rename, or read failed
    Io(std::io::Error),
    /// The roll on disk is not valid JSON, or its `version` is one this
    /// daemon does not know how to restore (carries the parse/version
    /// message)
    Parse(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoParent(path) => {
                write!(f, "roll path `{}` has no parent directory", path.display())
            }
            Self::Encode(err) => write!(f, "muster roll failed to serialize: {err}"),
            Self::Io(err) => write!(f, "muster roll I/O failed: {err}"),
            Self::Parse(msg) => write!(f, "muster roll is unreadable: {msg}"),
        }
    }
}

impl core::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Encode(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::NoParent(_) | Self::Parse(_) => None,
        }
    }
}

impl From<serde_json::Error> for SnapshotError {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode(source)
    }
}

impl From<std::io::Error> for SnapshotError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

/// Writes `snapshot` to `path` atomically: a temp file in the same directory,
/// since `rename(2)` is atomic only within one filesystem, `fsync`ed, renamed
/// over `path`, then on unix the directory `fsync`ed so the rename survives a
/// power cut (a no-op on Windows). The temp file is owner-only (unix mode
/// 0600) and `persist` keeps that mode: the roll stores app `env` verbatim,
/// the one place shep writes secrets to disk.
///
/// # Errors
/// - [`SnapshotError::NoParent`]: the roll path has no directory to write into.
/// - [`SnapshotError::Encode`]: the roll failed to serialize.
/// - [`SnapshotError::Io`]: the temp file, fsync, or rename failed.
pub(crate) fn write_atomic(path: &Path, snapshot: &FlockSnapshot) -> Result<(), SnapshotError> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| SnapshotError::NoParent(path.to_path_buf()))?;
    let json = serde_json::to_vec_pretty(snapshot)?;

    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(&json)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|err| SnapshotError::Io(err.error))?;

    // The `sync_all` above made the CONTENTS durable; this makes the rename
    // that published them durable. See `shep_core::atomic_file`.
    shep_core::atomic_file::sync_dir(parent)?;
    Ok(())
}

/// Reads and validates a muster roll written by `write_atomic`.
///
/// Public only for `tests/daemon_e2e.rs`, which asserts on the roll a live
/// daemon wrote; the daemon's own restore path calls it from inside `boot`.
///
/// # Errors
/// - [`SnapshotError::Io`]: the roll could not be read.
/// - [`SnapshotError::Parse`]: invalid JSON, or a schema version this daemon
///   does not know.
pub fn read(path: &Path) -> Result<FlockSnapshot, SnapshotError> {
    let bytes = std::fs::read(path)?;
    let snapshot: FlockSnapshot =
        serde_json::from_slice(&bytes).map_err(|err| SnapshotError::Parse(err.to_string()))?;
    if snapshot.version != SNAPSHOT_VERSION {
        return Err(SnapshotError::Parse(format!(
            "roll schema version {} is not one this daemon knows (expected {SNAPSHOT_VERSION})",
            snapshot.version
        )));
    }
    Ok(snapshot)
}

/// What a `muster` should put back: every member, which of them to start, and
/// the ones that failed re-validation
#[derive(Debug)]
pub(crate) struct Restorable {
    /// Every entry that still normalizes, in roll order. The flock is a
    /// membership list, so all of these are registered whether or not they
    /// run.
    pub(crate) members: Vec<ResolvedApp>,
    /// The subset of [`Self::members`] that was up when the roll was written
    /// and still opts into `autostart`.
    pub(crate) to_start: Vec<ResolvedApp>,
    /// Sheep name, and why its saved config failed to normalize
    pub(crate) rejected: Vec<(String, NormalizeError)>,
}

/// Splits a loaded [`FlockSnapshot`] into members, the subset to start, and
/// the entries rejected on re-validation.
///
/// Membership survives everything but `delete`: a sheep stopped when the roll
/// was written comes back registered and `Stopped`. What to START is a
/// separate question, answered by `instances_running > 0 && autostart`.
///
/// The roll is a file a human can edit, so every entry is run back through
/// [`normalize()`] like peer input, and a bad one is collected into `rejected`
/// rather than aborting the muster.
#[must_use]
pub(crate) fn restorable(snapshot: FlockSnapshot) -> Restorable {
    let mut members = Vec::new();
    let mut to_start = Vec::new();
    let mut rejected = Vec::new();
    for saved in snapshot.apps {
        let name = saved.app.name.clone();
        let was_up = saved.instances_running > 0;
        let autostart = saved.app.autostart;
        match normalize(saved.app) {
            Ok(resolved) => {
                if was_up && autostart {
                    to_start.push(resolved.clone());
                }
                members.push(resolved);
            }
            Err(err) => rejected.push((name, err)),
        }
    }
    Restorable {
        members,
        to_start,
        rejected,
    }
}

/// Reads the muster roll and starts every app it restores, returning the
/// names it restored.
///
/// The daemon's one restore path: `boot` runs it under `--restore`, the
/// `Muster` request runs it for an operator. An app the flock already has is
/// left where it stands, still counted as restored, which makes the verb
/// idempotent; starting it again is not a no-op, since
/// [`instance_slots`](crate::assemble::instance_slots) takes the lowest free
/// slot. A missing roll is not an error; an unparseable one is reported.
///
/// # Errors
/// - [`SnapshotError`]: the roll exists but could not be read or parsed.
pub(crate) async fn muster(
    path: &Path,
    registry: &FlockRegistry,
    supervisor: &SupervisorHandle,
    events: &Bus,
    dogs: &[String],
    boot_first_dogs: &[String],
) -> Result<Vec<String>, SnapshotError> {
    let saved = match read(path) {
        Ok(saved) => saved,
        Err(SnapshotError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(err) => return Err(err),
    };
    let restorable = restorable(saved);
    for (name, err) in &restorable.rejected {
        tracing::warn!(name, %err, "muster roll entry rejected on restore");
    }
    if restorable.members.is_empty() {
        return Ok(Vec::new());
    }
    let restored: Vec<String> = restorable
        .members
        .iter()
        .map(|app| app.config().name.clone())
        .collect();

    // A supervisor that cannot be listed has no flock left to collide with,
    // and the `start` below announces its own failure.
    let running = supervisor.list_checked().await.unwrap_or_default();
    let known = |app: &ResolvedApp| running.iter().any(|info| info.name == app.config().name);

    // Membership for every entry `start` will not bring up, so a sheep saved
    // while stopped comes back listed. The ones being started are excluded:
    // registering them at rest first would leave an idle `instance: 0` entry
    // for `start` to allocate around, and the sheep would show up twice.
    let starting: Vec<&str> = restorable
        .to_start
        .iter()
        .map(|app| app.config().name.as_str())
        .collect();
    let members: Vec<ResolvedApp> = restorable
        .members
        .iter()
        .filter(|app| !known(app) && !starting.contains(&app.config().name.as_str()))
        .cloned()
        .collect();
    if !members.is_empty() {
        registry.record(&members);
        if let Err(err) = supervisor.register_at_rest(members).await {
            tracing::warn!(%err, "muster roll restore could not register one or more apps");
        }
    }

    let to_start: Vec<ResolvedApp> = restorable
        .to_start
        .into_iter()
        .filter(|app| !known(app))
        .collect();
    if to_start.is_empty() {
        return Ok(restored);
    }
    // Recorded whether or not the stages fully succeed: already-registered
    // entries must persist even when a later spawn fails. Only what this call
    // starts, so an app left where it stands keeps the config it is actually
    // running under.
    registry.record(&to_start);

    let plan = crate::boot_order::plan_for(&to_start, dogs, boot_first_dogs);
    warn_about_the_graph(&plan, &to_start, &restorable.members);
    // `BatchPolicy::PerApp`, not `AllOrNothing`: a whole-batch refusal over an
    // app whose script provably is not there is right for an operator typing
    // `shep start` and wrong at an unattended boot, where a binary missing
    // after a rebuild would cost the machine its entire flock.
    crate::boot_order::start_in_stages(&plan, &to_start, supervisor, events, BatchPolicy::PerApp)
        .await;
    Ok(restored)
}

/// Reports every way the roll's dependency graph is not what it says it is.
///
/// Nothing here refuses. A restore runs on a machine nobody is watching, so
/// each of these three brings the flock up and says what it did instead.
fn warn_about_the_graph(plan: &BootPlan, to_start: &[ResolvedApp], members: &[ResolvedApp]) {
    // A member the roll holds but this restore will not start. Named
    // separately from the unresolved edges below, since "the sheep exists and
    // opted out" is a different thing for an operator to do about than "the
    // name is a typo".
    let opted_out: BTreeSet<&str> = members
        .iter()
        .filter(|app| !app.config().autostart)
        .map(|app| app.config().name.as_str())
        .collect();
    // Every name the roll can put back, whether or not this restore starts
    // it. The plan is built from `to_start` alone, so a dependency already
    // running, saved stopped, or opted out is absent from the graph and reads
    // as unresolved. On `shep muster` against a live flock that is the whole
    // flock, and the warning below would fire on the commonest path there is.
    let restorable: BTreeSet<&str> = members
        .iter()
        .map(|app| app.config().name.as_str())
        .collect();

    for unresolved in &plan.unresolved {
        // `opted_out` is a subset of this, and has its own warning below.
        if restorable.contains(unresolved.missing.as_str()) {
            continue;
        }
        tracing::warn!(
            sheep = %unresolved.dependent,
            missing = %unresolved.missing,
            "a dependency names nothing this flock has; starting without it"
        );
    }
    for app in to_start {
        for target in &app.config().depends_on {
            if opted_out.contains(target.as_str()) {
                tracing::warn!(
                    sheep = %app.config().name,
                    dependency = %target,
                    "the dependency sets autostart = false; starting without it"
                );
            }
        }
    }
    for cycle in &plan.cycles {
        tracing::warn!(
            cycle = %render_cycle(cycle),
            "a dependency cycle; those sheep start last, in no particular order"
        );
    }
    // `BootPlan::cycles` carries ONE representative path per knot, so a sheep
    // inside the knot but off that path is just as stuck and is never named by
    // the warning above. The cyclic stage is every one of them.
    //
    // Silent when the paths already named everybody, which is the plain
    // two-node cycle: `plan` puts every knot in one stage, so this line is
    // flock-wide and would otherwise repeat the line above verbatim. Per-knot
    // membership is a `BootPlan` field the graph module does not expose.
    if let Some(stuck) = cyclic_stage(plan) {
        let named: BTreeSet<&str> = plan.cycles.iter().flatten().map(String::as_str).collect();
        if stuck.iter().any(|name| !named.contains(name.as_str())) {
            let names: Vec<&str> = stuck.iter().map(String::as_str).collect();
            tracing::warn!(
                stuck = ?names,
                "every sheep a dependency cycle holds; each of them starts last"
            );
        }
    }
}

/// The stage the plan lifted every cyclic node into, if the graph has a cycle.
///
/// [`BootPlan`] does not label that stage, so it is found by the property that
/// identifies it: a name the plan reports in a cycle appears in no other one.
fn cyclic_stage(plan: &BootPlan) -> Option<&Vec<String>> {
    let member = plan.cycles.first()?.first()?;
    plan.stages.iter().find(|stage| stage.contains(member))
}

/// True for lifecycle transitions the roll cares about; false for log
/// traffic and daemon-wide notices, which must not trigger a rewrite.
fn is_state_change(event: &BusEvent) -> bool {
    matches!(event, BusEvent::Process { .. })
}

/// Handle to the debounced writer task
#[derive(Debug)]
pub(crate) struct SnapshotWriter {
    handle: JoinHandle<()>,
    /// Read only by [`Self::writes`], which carries the same `allow`.
    #[allow(dead_code, reason = "read by this crate's own tests through `writes`")]
    writes: Arc<AtomicU64>,
}

impl SnapshotWriter {
    /// Completed roll writes since boot
    ///
    /// The metrics dog reads this off the wire, so the only callers are this
    /// module's tests and it is dead in a non-test build.
    // A trivial atomic load, not per-frame hot: #[inline], never #[inline(always)].
    #[inline]
    #[must_use]
    #[allow(dead_code, reason = "called by this module's own tests")]
    pub(crate) fn writes(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    /// Stops the writer and waits for it (the caller then owns roll timing)
    pub(crate) async fn stop(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

/// Spawns the debounced muster-roll writer.
///
/// Coalesces bursts of lifecycle events (spec §13.4: one restart storm, one
/// write) into a single [`write_atomic`] call per [`SNAPSHOT_DEBOUNCE_MS`]
/// window. Log traffic never resets or starts the debounce timer.
///
/// Woken by [`FlockRegistry`]'s own writes as well as by the bus, so a config
/// change that parks a field reaches the file without a process having moved.
pub(crate) fn spawn_snapshot_writer(
    path: PathBuf,
    supervisor: SupervisorHandle,
    registry: FlockRegistry,
    events: broadcast::Receiver<SharedEvent>,
) -> SnapshotWriter {
    let writes = Arc::new(AtomicU64::new(0));
    let task_writes = Arc::clone(&writes);
    let dirty = Arc::clone(&registry.dirty);
    let handle = tokio::spawn(run_writer(
        path,
        supervisor,
        registry,
        events,
        dirty,
        task_writes,
    ));
    SnapshotWriter { handle, writes }
}

/// The writer's actor loop. Cancel-safe: the debounce deadline is recomputed
/// from the stored `Option<Instant>` every iteration, so losing the `select!`
/// race never extends the window.
async fn run_writer(
    path: PathBuf,
    supervisor: SupervisorHandle,
    registry: FlockRegistry,
    mut events: broadcast::Receiver<SharedEvent>,
    dirty: Arc<Notify>,
    writes: Arc<AtomicU64>,
) {
    let mut deadline: Option<Instant> = None;
    loop {
        tokio::select! {
            received = events.recv() => match received {
                // Only lifecycle events change the roll; log lines must not
                // rewrite a file once per output line.
                Ok(event) => if is_state_change(&event) && deadline.is_none() {
                    deadline = Some(Instant::now() + Duration::from_millis(SNAPSHOT_DEBOUNCE_MS));
                },
                // A lag may have swallowed a lifecycle event: assume dirty.
                Err(RecvError::Lagged(_)) => if deadline.is_none() {
                    deadline = Some(Instant::now() + Duration::from_millis(SNAPSHOT_DEBOUNCE_MS));
                },
                Err(RecvError::Closed) => break,
            },
            // A config write that parks a field moves no process, so the bus
            // says nothing and only this arm reaches the file before the next
            // unrelated lifecycle event or a graceful shutdown.
            () = dirty.notified() => if deadline.is_none() {
                deadline = Some(Instant::now() + Duration::from_millis(SNAPSHOT_DEBOUNCE_MS));
            },
            () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                deadline = None;
                write_now(&path, &supervisor, &registry, &writes).await;
            }
        }
    }
}

// The write is a few KiB to a local file once per debounce window;
// `spawn_blocking` would buy a task hop and nothing else.
async fn write_now(
    path: &Path,
    supervisor: &SupervisorHandle,
    registry: &FlockRegistry,
    writes: &AtomicU64,
) {
    // Engine gone: there is nothing left to record and the shutdown path has
    // already written the final roll.
    let Ok(infos) = supervisor.list_checked().await else {
        return;
    };
    let roll = registry.roll(&infos, crate::now_ms()); // lock released before any IO
    match write_atomic(path, &roll) {
        Ok(()) => {
            writes.fetch_add(1, Ordering::SeqCst);
        }
        Err(err) => tracing::warn!(%err, "muster roll write failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{ProcScript, ScriptedRunner};
    use crate::supervisor::spawn_supervisor;
    use crate::testing::{capture_logs, test_paths};
    use shep_core::config::graph::{BootNode, NodeKind, plan};
    use shep_core::config::{AppConfig, normalize};
    use shep_core::protocol::{BusEvent, ProcessEventKind, ProcessInfo};
    use shep_core::status::ProcStatus;
    use shep_core::values::UpDuration;
    use std::time::Duration;

    fn info(id: u32, name: &str, status: ProcStatus) -> ProcessInfo {
        ProcessInfo::builder(id, name, status)
            .pid(Some(1000 + id))
            .out_file(Some(format!("/logs/{name}-0-out.log")))
            .err_file(Some(format!("/logs/{name}-0-err.log")))
            .build()
    }

    // fails if `is_running` starts counting `ProcStatus::Stopping`, the status
    // a reload's drainee wears once its replacement is spawned. Both share one
    // instance slot for the swap, so counting the drainee would roll a
    // one-instance app at two.
    #[test]
    fn roll_counts_running_instances_and_prunes_deleted_names() {
        let registry = FlockRegistry::new();
        let web = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        let job = normalize(AppConfig::minimal("job", "./job")).unwrap();
        registry.record(&[web, job]);

        let infos = [
            info(0, "web", ProcStatus::Online),
            info(1, "web", ProcStatus::WaitingRestart),
            info(2, "web", ProcStatus::Stopped),
            info(3, "web", ProcStatus::Stopping),
        ]; // `job` was deleted: no entries left
        let roll = registry.roll(&infos, 1_700_000_000_000);

        assert_eq!(roll.version, SNAPSHOT_VERSION);
        assert_eq!(roll.saved_at_ms, 1_700_000_000_000);
        assert_eq!(roll.apps.len(), 1, "a name with no live entry is pruned");
        assert_eq!(roll.apps[0].app.name, "web");
        // online + waiting-restart; neither the stopped one nor the drainee
        assert_eq!(roll.apps[0].instances_running, 2);
        // The prune is sticky: a second roll must not resurrect `job`.
        assert_eq!(registry.roll(&infos, 0).apps.len(), 1);
    }

    #[test]
    fn write_atomic_round_trips_with_no_leftovers() {
        // The `0600` half lives in `write_atomic_is_owner_only_on_unix`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flock.json");
        let registry = FlockRegistry::new();
        registry.record(&[normalize(AppConfig::minimal("web", "./srv")).unwrap()]);
        let roll = registry.roll(&[info(0, "web", ProcStatus::Online)], 42);

        write_atomic(&path, &roll).unwrap();
        write_atomic(&path, &roll).unwrap(); // overwriting keeps the guarantees

        assert_eq!(read(&path).unwrap(), roll);
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "no temp file may survive a completed write"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_is_owner_only_on_unix() {
        // The roll stores app env verbatim (spec §10): owner-only, always.
        // Unix-gated because `0600` has no Windows ACL equivalent.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flock.json");
        let registry = FlockRegistry::new();
        registry.record(&[normalize(AppConfig::minimal("web", "./srv")).unwrap()]);
        let roll = registry.roll(&[info(0, "web", ProcStatus::Online)], 42);

        write_atomic(&path, &roll).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the muster roll holds app env in cleartext");
    }

    #[test]
    fn read_rejects_corrupt_json_and_unknown_schema_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flock.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(read(&path), Err(SnapshotError::Parse { .. })));

        let future = format!(
            "{{\"version\":{},\"saved_at_ms\":0,\"apps\":[]}}",
            SNAPSHOT_VERSION + 1
        );
        std::fs::write(&path, future.as_bytes()).unwrap();
        assert!(matches!(read(&path), Err(SnapshotError::Parse { .. })));
    }

    /// Restoring only the running ones would make `shep stop` destructive
    /// across a daemon restart: the sheep would leave the flock entirely.
    #[test]
    fn restorable_keeps_every_member_and_starts_only_what_was_up() {
        let mut stopped = AppConfig::minimal("stopped", "./s");
        stopped.instances = 1;
        let mut opted_out = AppConfig::minimal("manual", "./m");
        opted_out.autostart = false;

        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![
                SavedApp {
                    app: AppConfig::minimal("web", "./srv"),
                    instances_running: 2,
                },
                SavedApp {
                    app: stopped,
                    instances_running: 0,
                },
                SavedApp {
                    app: opted_out,
                    instances_running: 1,
                },
            ],
        };
        let restorable = restorable(roll);

        let members: Vec<&str> = restorable
            .members
            .iter()
            .map(|a| a.config().name.as_str())
            .collect();
        assert_eq!(
            members,
            ["web", "stopped", "manual"],
            "every entry that normalizes belongs to the flock, running or not"
        );

        let starting: Vec<&str> = restorable
            .to_start
            .iter()
            .map(|a| a.config().name.as_str())
            .collect();
        assert_eq!(
            starting,
            ["web"],
            "only the sheep that was up and opts into autostart is started"
        );
        assert!(restorable.rejected.is_empty());
    }

    /// So a later `restart` has something to find.
    #[test]
    fn a_sheep_saved_while_stopped_is_still_a_member() {
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("zeus-auth", "./zeus-auth"),
                instances_running: 0,
            }],
        };
        let restorable = restorable(roll);
        assert_eq!(restorable.members.len(), 1, "stopping is not forgetting");
        assert_eq!(restorable.members[0].config().name, "zeus-auth");
        assert!(
            restorable.to_start.is_empty(),
            "but it stays stopped: it was not running when the roll was written"
        );
    }

    #[test]
    fn restorable_reports_a_hand_edited_invalid_app_instead_of_aborting() {
        let mut broken = AppConfig::minimal("broken", "./b");
        broken.instances = 0; // someone edited the roll
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![
                SavedApp {
                    app: broken,
                    instances_running: 1,
                },
                SavedApp {
                    app: AppConfig::minimal("web", "./srv"),
                    instances_running: 1,
                },
            ],
        };
        let restorable = restorable(roll);
        assert_eq!(
            restorable.members.len(),
            1,
            "one bad entry must not sink the muster"
        );
        assert_eq!(
            restorable.rejected,
            vec![(
                "broken".to_string(),
                shep_core::config::NormalizeError::ZeroInstances
            )]
        );
    }

    #[test]
    fn debug_does_not_leak_env_values() {
        // The roll carries env, and its Debug lands in daemon logs.
        let mut app = AppConfig::minimal("web", "./srv");
        app.env
            .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app,
                instances_running: 1,
            }],
        };
        let rendered = format!("{roll:?}");
        assert!(!rendered.contains("postgres://secret"), "{rendered}");
        assert!(rendered.contains("<1 vars>"), "{rendered}");
    }

    /// fails if `muster` starts an app the roll says was down, skips one it
    /// says was up, or drops either from the flock. All three in one case: an
    /// inverted restore rule would pass any one alone.
    #[tokio::test(start_paused = true)]
    async fn muster_restores_both_and_starts_only_the_one_that_was_up() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![
                SavedApp {
                    app: AppConfig::minimal("up", "./srv"),
                    instances_running: 1,
                },
                SavedApp {
                    app: AppConfig::minimal("down", "./srv"),
                    instances_running: 0,
                },
            ],
        };
        write_atomic(&paths.snapshot, &roll).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();

        let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();
        assert_eq!(
            restored,
            vec!["up".to_string(), "down".to_string()],
            "both are restored to the flock; only one of them runs"
        );

        let mut listed = handle.list().await;
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let seen: Vec<(&str, ProcStatus)> =
            listed.iter().map(|i| (i.name.as_str(), i.status)).collect();
        assert_eq!(
            seen,
            vec![("down", ProcStatus::Stopped), ("up", ProcStatus::Online)],
            "the sheep that was down is listed and stopped, not missing"
        );
        handle.shutdown().await;
    }

    /// fails if a bad entry that is not LAST takes every app after it down.
    ///
    /// `FlockRegistry` is a `BTreeMap`, so the roll is alphabetical and
    /// `restorable` preserves that order: the `a-`/`b-`/`c-` names here make
    /// the roll order the production order, with the wreck in the middle.
    ///
    /// One `muster` call: `shep muster`'s CLI path calls twice and the second
    /// re-registers what the first lost, so only the single unattended restore
    /// a `shep startup` unit performs can show this. `failing_to_spawn` is
    /// load-bearing, since scripts are consumed in spawn order.
    #[tokio::test(start_paused = true)]
    async fn a_bad_saved_app_does_not_take_the_apps_after_it_down() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let saved = |name: &str| SavedApp {
            app: AppConfig::minimal(name, "./srv"),
            instances_running: 1,
        };
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![saved("a-good"), saved("b-bad"), saved("c-good")],
        };
        write_atomic(&paths.snapshot, &roll).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            // Two scripts for the two that must come up. `b-bad` consumes
            // none, so a run that let it take one would starve `c-good` and
            // fail here for a reason of its own.
            ScriptedRunner::new(vec![ProcScript::never_exits(); 2]).failing_to_spawn(&["b-bad"]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();

        let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();
        assert_eq!(
            restored,
            vec![
                "a-good".to_string(),
                "b-bad".to_string(),
                "c-good".to_string()
            ]
        );

        let mut listed = handle.list().await;
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let seen: Vec<(&str, ProcStatus)> =
            listed.iter().map(|i| (i.name.as_str(), i.status)).collect();
        assert_eq!(
            seen,
            vec![
                ("a-good", ProcStatus::Online),
                ("b-bad", ProcStatus::Errored),
                ("c-good", ProcStatus::Online),
            ],
            "every app after the broken one must still get its turn, and the \
             broken one must be visible rather than absent"
        );
        handle.shutdown().await;
    }

    /// fails if ONE saved app that cannot start keeps the rest of the flock
    /// down at the next boot.
    ///
    /// `muster` starts under `BatchPolicy::PerApp`, not `AllOrNothing`,
    /// because the pre-registration check refuses a whole batch over one app
    /// whose script is gone, which at an unattended boot costs the machine its
    /// flock.
    ///
    /// `refusing` is load-bearing: `ScriptedRunner`'s `preflight` answers
    /// `Unknown` for everything, so the validating pass refuses nothing on its
    /// own. `failing_to_spawn` is what makes `gone` land as a visible
    /// `Errored` row rather than vanishing, and it names the app rather than
    /// starving it of a script: the restore starts a stage's members in the
    /// plan's order, not the roll's, and `gone` sorts first.
    #[tokio::test(start_paused = true)]
    async fn one_unstartable_saved_app_does_not_keep_the_rest_of_the_flock_down() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![
                SavedApp {
                    app: AppConfig::minimal("good", "./srv"),
                    instances_running: 1,
                },
                SavedApp {
                    app: AppConfig::minimal("gone", "./deleted-by-a-rebuild"),
                    instances_running: 1,
                },
            ],
        };
        write_atomic(&paths.snapshot, &roll).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()])
                .refusing(&["gone"])
                .failing_to_spawn(&["gone"]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();

        let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();
        assert_eq!(restored, vec!["good".to_string(), "gone".to_string()]);

        let mut listed = handle.list().await;
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let seen: Vec<(&str, ProcStatus)> =
            listed.iter().map(|i| (i.name.as_str(), i.status)).collect();
        assert_eq!(
            seen,
            vec![("gone", ProcStatus::Errored), ("good", ProcStatus::Online)],
            "the app that could still run must come up, and the one that \
             could not must be visible rather than absent"
        );
        handle.shutdown().await;
    }

    /// fails if `muster` starts an app the flock already has.
    ///
    /// `instance_slots` hands a second `Start` of a one-instance app the next
    /// free slot, so an unconditional muster would leave it running two. The
    /// single script makes that visible: the flock's own `web` consumes it, so
    /// a duplicate lands as `Errored`.
    #[tokio::test(start_paused = true)]
    async fn muster_leaves_an_app_the_flock_already_has_where_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let roll = FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: vec![SavedApp {
                app: AppConfig::minimal("web", "./srv"),
                instances_running: 1,
            }],
        };
        write_atomic(&paths.snapshot, &roll).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();
        let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        registry.record(std::slice::from_ref(&app));
        handle.start(vec![app]).await.unwrap();

        let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();
        assert_eq!(
            restored,
            vec!["web".to_string()],
            "the roll's app is restored whether or not this call is what started it"
        );
        let listed = handle.list().await;
        assert_eq!(listed.len(), 1, "one instance of a one-instance app");
        assert_eq!(listed[0].status, ProcStatus::Online);
        handle.shutdown().await;
    }

    /// A muster roll holding `apps`, every one of them recorded as running.
    fn roll_of(apps: Vec<AppConfig>) -> FlockSnapshot {
        FlockSnapshot {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps: apps
                .into_iter()
                .map(|app| SavedApp {
                    app,
                    instances_running: 1,
                })
                .collect(),
        }
    }

    /// Every `process.start` name waiting on `rx`, in the order the bus
    /// carried them.
    fn start_order(rx: &mut broadcast::Receiver<SharedEvent>) -> Vec<String> {
        let mut order = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let BusEvent::Process {
                event: ProcessEventKind::Start,
                info,
                ..
            } = &*event
            {
                order.push(info.name.clone());
            }
        }
        order
    }

    /// fails if the restore still hands the whole roll over as one batch. The
    /// roll is written `api` first, which is the order that starts a sheep
    /// before the one it waits for.
    #[tokio::test]
    async fn a_restore_starts_the_roll_in_dependency_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let mut db = AppConfig::minimal("db", "./srv");
        db.listen_timeout = UpDuration::from_millis(50);
        let mut api = AppConfig::minimal("api", "./srv");
        api.depends_on = vec!["db".to_string()];
        write_atomic(&paths.snapshot, &roll_of(vec![api, db])).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits(); 2]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();
        let mut seen = events.subscribe();

        muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();

        assert_eq!(
            start_order(&mut seen),
            vec!["db", "api"],
            "the roll's order must not decide boot order"
        );
        handle.shutdown().await;
    }

    /// fails if a cycle refuses the restore, which would strand an unattended
    /// boot on a typo nobody is there to read.
    #[tokio::test]
    async fn a_cyclic_roll_still_brings_the_flock_up() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let mut a = AppConfig::minimal("a", "./srv");
        a.depends_on = vec!["b".to_string()];
        let mut b = AppConfig::minimal("b", "./srv");
        b.depends_on = vec!["a".to_string()];
        write_atomic(&paths.snapshot, &roll_of(vec![a, b])).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits(); 2]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();

        let restored = muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .expect("a cycle must not refuse the restore");

        assert_eq!(restored, vec!["a".to_string(), "b".to_string()]);
        let mut listed = handle.list().await;
        listed.sort_by(|left, right| left.name.cmp(&right.name));
        let seen: Vec<(&str, ProcStatus)> = listed
            .iter()
            .map(|info| (info.name.as_str(), info.status))
            .collect();
        assert_eq!(
            seen,
            vec![("a", ProcStatus::Online), ("b", ProcStatus::Online)],
            "both members of the knot run; neither waits for the other"
        );
        handle.shutdown().await;
    }

    /// fails if `depends_on` overrides `autostart`, which would let one app's
    /// file start a sheep another app's file said not to start.
    #[tokio::test]
    async fn a_dependency_with_autostart_off_is_warned_about_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        let mut db = AppConfig::minimal("db", "./srv");
        db.autostart = false;
        let mut api = AppConfig::minimal("api", "./srv");
        api.depends_on = vec!["db".to_string()];
        write_atomic(&paths.snapshot, &roll_of(vec![db, api])).unwrap();

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();
        let mut seen = events.subscribe();

        muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
            .await
            .unwrap();

        assert_eq!(
            start_order(&mut seen),
            vec!["api"],
            "db opted out; api starts without it"
        );
        handle.shutdown().await;
    }

    /// An app named `name` that waits for `deps`, normalized.
    fn dependant(name: &str, deps: &[&str]) -> ResolvedApp {
        let mut app = AppConfig::minimal(name, "./srv");
        app.depends_on = deps.iter().map(|dep| (*dep).to_string()).collect();
        normalize(app).expect("a minimal app with edges normalizes")
    }

    /// fails if a dependency the roll still holds is reported as naming
    /// nothing this flock has. The plan is built from `to_start` alone, so on
    /// `shep muster` against a live flock every already-running dependency is
    /// unresolved, and this warning fired on the commonest path there is.
    ///
    /// The second half is the control: a name the roll does not hold either
    /// really is missing, and must still be said out loud.
    #[test]
    fn a_dependency_the_roll_still_holds_is_not_called_missing() {
        let api = dependant("api", &["db"]);
        let db = normalize(AppConfig::minimal("db", "./srv")).expect("a minimal app normalizes");
        let to_start = vec![api.clone()];
        let plan = crate::boot_order::plan_for(&to_start, &[], &[]);
        assert_eq!(
            plan.unresolved.len(),
            1,
            "the plan only ever sees what this restore starts"
        );

        let held = capture_logs(|| warn_about_the_graph(&plan, &to_start, &[db, api.clone()]));
        assert!(!held.contains("names nothing this flock has"), "{held}");

        let absent = capture_logs(|| warn_about_the_graph(&plan, &to_start, &[api]));
        assert!(absent.contains("names nothing this flock has"), "{absent}");
    }

    /// fails if a plain two-node cycle prints its membership twice. `plan`
    /// puts every knot into one stage, so the membership line is flock-wide,
    /// and on two nodes it repeats the representative path verbatim.
    ///
    /// The three-node knot is the control: there the path names two of the
    /// three, so the membership line is the only thing that names the third.
    #[test]
    fn a_two_node_knot_is_not_reported_twice_over() {
        let pair = vec![dependant("a", &["b"]), dependant("b", &["a"])];
        let plan = crate::boot_order::plan_for(&pair, &[], &[]);

        let two = capture_logs(|| warn_about_the_graph(&plan, &pair, &pair));
        assert!(
            !two.contains("every sheep a dependency cycle holds"),
            "{two}"
        );

        let trio = vec![
            dependant("a", &["b"]),
            dependant("b", &["a", "c"]),
            dependant("c", &["b"]),
        ];
        let plan = crate::boot_order::plan_for(&trio, &[], &[]);
        let three = capture_logs(|| warn_about_the_graph(&plan, &trio, &trio));
        assert!(
            three.contains("every sheep a dependency cycle holds"),
            "{three}"
        );
    }

    /// fails if the cycle warning names only the representative path. Every
    /// one of these three is stuck, and the path through the knot names two.
    #[test]
    fn the_cyclic_stage_names_every_sheep_in_the_knot() {
        let node = |name: &str, deps: &[&str]| BootNode {
            name: name.to_string(),
            depends_on: deps.iter().map(|dep| (*dep).to_string()).collect(),
            kind: NodeKind::Sheep,
        };
        let plan = plan(&[node("a", &["b"]), node("b", &["a", "c"]), node("c", &["b"])]);
        assert_eq!(plan.cycles.len(), 1, "one knot, one representative path");
        assert!(
            plan.cycles[0].len() < 3,
            "the point of the extra report is a member the path leaves out"
        );

        assert_eq!(
            cyclic_stage(&plan),
            Some(&vec!["a".to_string(), "b".to_string(), "c".to_string()]),
        );
    }

    /// fails if a missing roll becomes an error. A fresh `$SHEP_HOME` has
    /// none, and a daemon that refused over it could not boot on a clean
    /// machine.
    #[tokio::test(start_paused = true)]
    async fn a_missing_roll_restores_nothing_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(paths.snapshot.parent().unwrap()).unwrap();
        assert!(!paths.snapshot.exists());

        let (events, _rx) = crate::bus::test_bus(64);
        let handle = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();

        assert_eq!(
            muster(&paths.snapshot, &registry, &handle, &events, &[], &[])
                .await
                .unwrap(),
            Vec::<String>::new()
        );
        assert!(handle.list().await.is_empty());
        handle.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn writer_coalesces_a_burst_into_one_write() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(&paths.home).unwrap();
        let (events, _keep) = crate::bus::test_bus(64);
        let supervisor = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();
        let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        registry.record(std::slice::from_ref(&app));
        supervisor.start(vec![app]).await.unwrap();

        // Subscribing here means the start's own events are already behind us.
        let writer = spawn_snapshot_writer(
            paths.snapshot.clone(),
            supervisor.clone(),
            registry,
            events.subscribe(),
        );
        for event in [
            ProcessEventKind::Exit,
            ProcessEventKind::Restart,
            ProcessEventKind::Online,
        ] {
            events
                .send(
                    BusEvent::Process {
                        event,
                        info: info(0, "web", ProcStatus::Online),
                        manually: false,
                        at_ms: 0,
                    }
                    .into(),
                )
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS + 1)).await;

        assert_eq!(writer.writes(), 1, "one debounce window is one write");
        let roll = read(&paths.snapshot).unwrap();
        assert_eq!(roll.apps.len(), 1);
        assert_eq!(roll.apps[0].instances_running, 1);
        writer.stop().await;
    }

    /// fails if the bus is the writer's only schedule. A config write that
    /// parks a field moves no process, so it publishes no
    /// [`BusEvent::Process`], and the registry's own recording is the roll's
    /// only route to disk before the next unrelated lifecycle event or a
    /// graceful shutdown.
    #[tokio::test(start_paused = true)]
    async fn writer_schedules_a_write_for_a_recording_the_bus_says_nothing_about() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(&paths.home).unwrap();
        let (events, _keep) = crate::bus::test_bus(64);
        let supervisor = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let registry = FlockRegistry::new();
        let app = normalize(AppConfig::minimal("web", "./srv")).unwrap();
        supervisor.start(vec![app.clone()]).await.unwrap();

        // Subscribing here means the start's own events are already behind us,
        // so the bus has nothing left to say about this flock.
        let writer = spawn_snapshot_writer(
            paths.snapshot.clone(),
            supervisor.clone(),
            registry.clone(),
            events.subscribe(),
        );
        assert!(!paths.snapshot.exists(), "nothing has recorded yet");

        registry.record(std::slice::from_ref(&app));
        tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS + 1)).await;

        assert_eq!(writer.writes(), 1, "a recording leaves the roll dirty");
        let roll = read(&paths.snapshot).unwrap();
        assert_eq!(roll.apps.len(), 1);
        writer.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn writer_ignores_log_traffic() {
        let dir = tempfile::tempdir().unwrap();
        let paths = test_paths(&dir);
        std::fs::create_dir_all(&paths.home).unwrap();
        let (events, _keep) = crate::bus::test_bus(64);
        let supervisor = spawn_supervisor(
            ScriptedRunner::new(vec![ProcScript::never_exits()]),
            paths.clone(),
            events.clone(),
        );
        let writer = spawn_snapshot_writer(
            paths.snapshot.clone(),
            supervisor,
            FlockRegistry::new(),
            events.subscribe(),
        );
        for id in 0..50 {
            events
                .send(
                    BusEvent::LogOut {
                        id,
                        line: "chatty".to_string(),
                    }
                    .into(),
                )
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(SNAPSHOT_DEBOUNCE_MS * 4)).await;
        assert_eq!(writer.writes(), 0, "log lines must never rewrite the roll");
        assert!(!paths.snapshot.exists());
        writer.stop().await;
    }
}
