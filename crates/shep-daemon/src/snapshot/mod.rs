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

impl FlockSnapshot {
    /// A roll holding `apps`, under the schema version this build writes.
    ///
    /// `saved_at_ms` is left at zero, which is what a roll nothing has
    /// written yet carries: `FlockRegistry::roll` is the path that stamps a
    /// real clock reading, and `restorable` reads only `instances_running`.
    /// The version constant is not public, so this is the only way a caller
    /// outside this crate can build a roll the daemon will accept rather
    /// than hard-coding a number that stops being true when the schema
    /// moves.
    #[must_use]
    pub fn with_apps(apps: Vec<SavedApp>) -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            saved_at_ms: 0,
            apps,
        }
    }
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

    /// Every registered sheep's name, with the names it says it waits for.
    ///
    /// Infallible, one lock and no normalize: the teardown builds its stop
    /// plan from this, and a fallible step on the one path that always runs
    /// would silently skip the staged stop instead of failing loudly.
    pub(crate) fn depends_on_by_name(&self) -> BTreeMap<String, Vec<String>> {
        self.apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(name, app)| (name.clone(), app.depends_on.clone()))
            .collect()
    }

    /// `name`'s `listen_timeout` and `graceful_timeout`, in that order, or
    /// `None` when no registered sheep has that name.
    ///
    /// The pair an ordered restart or reload bounds a stage's wait by. Read
    /// here because a `ProcessInfo` carries no timeout at all, and the
    /// alternative is a round trip to the actor per member of a stage.
    pub(crate) fn timeouts_of(&self, name: &str) -> Option<(Duration, Duration)> {
        self.apps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .map(|app| {
                (
                    app.listen_timeout.as_duration(),
                    app.graceful_timeout.as_duration(),
                )
            })
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
            saved_at_ms: now_ms,
            ..FlockSnapshot::with_apps(saved)
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
    shep_core::atomic_file::publish(tmp, path).map_err(SnapshotError::Io)
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
/// `dogs` names every dog this shepherd holds and `boot_first_dogs` the ones
/// `[daemon] boot_first_dogs` promotes ahead of the flock. Neither is spawned
/// here: they are the graph's dog nodes, and the split is what positions a
/// dog in the plan. Which of them [`warn_about_the_graph`] reports on is
/// decided by the live flock as well, since an unpromoted dog is already up
/// on every restore but the boot's own.
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
    warn_about_dogs_holding_sheep_names(&restorable.members, &running);

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
    warn_about_the_graph(
        &plan,
        &to_start,
        &restorable.members,
        &running,
        dogs,
        boot_first_dogs,
    );
    // `BatchPolicy::PerApp`, not `AllOrNothing`: a whole-batch refusal over an
    // app whose script provably is not there is right for an operator typing
    // `shep start` and wrong at an unattended boot, where a binary missing
    // after a rebuild would cost the machine its entire flock.
    // `PerApp` never answers `Err`; warned rather than discarded so a later
    // change of policy here cannot lose a failure silently.
    if let Err(err) = crate::boot_order::start_in_stages(
        &plan,
        &to_start,
        supervisor,
        events,
        BatchPolicy::PerApp,
    )
    .await
    {
        tracing::warn!(%err, "muster roll restore could not start one or more apps");
    }
    Ok(restored)
}

/// Warns for every saved sheep whose name a running dog has taken.
///
/// A `[daemon] boot_first_dogs` dog spawns before this restore and registers
/// under its own name against an empty flock, so [`muster`]'s `known` finds
/// the name running and filters the roll's sheep of that name out of both the
/// membership pass and the start. Nothing refuses that collision, and without
/// this line an operator loses a sheep in silence.
///
/// The name stays in the restored list. It really is in the flock, and
/// dropping it would report the loss as an absence on a terminal that has no
/// room to say why; the reply carries no per-app note to put the reason in.
fn warn_about_dogs_holding_sheep_names(members: &[ResolvedApp], running: &[ProcessInfo]) {
    for app in members {
        let name = app.config().name.as_str();
        let Some(source) = running
            .iter()
            .find(|info| info.name == name)
            .and_then(|info| info.dog.as_ref())
        else {
            continue;
        };
        tracing::warn!(
            name,
            dog = ?source,
            "a dog holds this name, so the roll's sheep of that name is not restored"
        );
    }
}

/// Whether `running` holds an entry of this name that has been spawned and
/// not given up on.
///
/// `Stopped` and `Errored` are the two that answer `false`: a dog registered
/// under either has no process behind it, so a sheep that waits for it really
/// does start without it. Every other status covers a live child, a
/// `WaitingRestart` gap between two of them included.
fn is_up(running: &[ProcessInfo], name: &str) -> bool {
    running.iter().any(|info| {
        info.name == name && !matches!(info.status, ProcStatus::Stopped | ProcStatus::Errored)
    })
}

/// Reports every way the roll's dependency graph is not what it says it is.
///
/// Nothing here refuses. A restore runs on a machine nobody is watching, so
/// each of these brings the flock up and says what it did instead: a
/// dependency nothing in the roll answers to, one that opted out of
/// `autostart`, one that is a dog starting after the flock rather than
/// before it, and a cycle.
///
/// `running` is the flock as it stands, which is what keeps the dog warning
/// from firing on a dog that is already up: this runs at an operator's
/// `shep muster` as well as at boot.
fn warn_about_the_graph(
    plan: &BootPlan,
    to_start: &[ResolvedApp],
    members: &[ResolvedApp],
    running: &[ProcessInfo],
    dogs: &[String],
    boot_first_dogs: &[String],
) {
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
    // A dog gets an ordinary graph position, but `boot` spawns dogs in two
    // groups and neither is at a stage boundary: the promoted ones before
    // this restore, the rest after every stage. So a dependency on an
    // unpromoted dog is ordered by the plan and by nothing else, and the
    // sheep starts while the dog is not running.
    let unpromoted: BTreeSet<&str> = dogs
        .iter()
        .map(String::as_str)
        .filter(|dog| !boot_first_dogs.iter().any(|first| first == dog))
        // A dog that is already up is not one this restore starts after the
        // flock, whatever the promotion list says. `muster` is the handler for
        // an operator's `Request::Muster` as well as the boot's restore, and
        // there both dog groups have been running for hours, so reading the
        // list alone would warn about every such edge on every `shep muster`.
        // The live flock, read the way `warn_about_dogs_holding_sheep_names`
        // reads it, is what tells the two apart.
        .filter(|dog| !is_up(running, dog))
        // A dog whose name a started sheep already holds is not a node at
        // all; the sheep is, and it is ordered properly. `plan_for` drops it
        // against exactly this list.
        .filter(|dog| !to_start.iter().any(|app| app.config().name == *dog))
        .collect();
    for app in to_start {
        for target in &app.config().depends_on {
            if unpromoted.contains(target.as_str()) {
                tracing::warn!(
                    sheep = %app.config().name,
                    dog = %target,
                    "the dependency is a dog outside [daemon] boot_first_dogs, \
                     which starts after the whole flock; starting without it"
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
    // flock-wide and would otherwise repeat the line above verbatim.
    // `BootPlan::knots` carries the membership per knot; this line wants the
    // flock-wide set, which the cyclic stage already is.
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
    use shep_core::config::AppConfig;
    use shep_core::protocol::ProcessInfo;
    use shep_core::status::ProcStatus;

    fn info(id: u32, name: &str, status: ProcStatus) -> ProcessInfo {
        ProcessInfo::builder(id, name, status)
            .pid(Some(1000 + id))
            .out_file(Some(format!("/logs/{name}-0-out.log")))
            .err_file(Some(format!("/logs/{name}-0-err.log")))
            .build()
    }

    mod graph;
    mod io;
    mod muster;
    mod roll;
    mod secrets;
    mod writer;

    /// So a later `restart` has something to find.
    #[test]
    fn a_sheep_saved_while_stopped_is_still_a_member() {
        let roll = FlockSnapshot::with_apps(vec![SavedApp {
            app: AppConfig::minimal("api-auth", "./api-auth"),
            instances_running: 0,
        }]);
        let restorable = restorable(roll);
        assert_eq!(restorable.members.len(), 1, "stopping is not forgetting");
        assert_eq!(restorable.members[0].config().name, "api-auth");
        assert!(
            restorable.to_start.is_empty(),
            "but it stays stopped: it was not running when the roll was written"
        );
    }
}
