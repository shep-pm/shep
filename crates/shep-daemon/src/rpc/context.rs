//! Per-connection state: [`RpcContext`], its deadline budget, and the
//! [`Outcome`] a handler hands back to the connection layer.
//!
//! [`KnownDogs`] is the set of dog names this shepherd may configure;
//! [`SavedRoll`] is what [`RpcContext::save_roll_now`] reports having
//! written.

use core::time::Duration;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use shep_core::paths::ShepPaths;
use shep_core::protocol::Reply;

use crate::bus::{Bus, TopicFilter};
use crate::host::HostState;
use crate::limits::stats::StatsState;
use crate::secrets::ProviderSecrets;
use crate::snapshot::{FlockRegistry, SnapshotError, write_atomic};
use crate::supervisor::SupervisorHandle;

/// Deadline applied when a client sends none (spec §6: 5s default).
pub(crate) const DEFAULT_DEADLINE_MS: u64 = 5_000;
/// Ceiling on a client-supplied deadline: a peer cannot pin a daemon task open.
pub(crate) const MAX_DEADLINE_MS: u64 = 60_000;

/// Every dog name this shepherd may hold a section for, running or not.
///
/// Seeded at boot from the CLI, which owns `shep.toml`. A
/// `Request::EnableDog` adds a name, so a dog adopted against a running
/// shepherd needs no reload. Never shrunk: `shep disable` leaves the
/// section in `dogs.toml`, where a disabled dog still wants configuring.
/// A set because the only question asked is membership, behind a mutex
/// because every connection holds a clone.
#[derive(Debug, Clone)]
pub(crate) struct KnownDogs {
    names: Arc<Mutex<BTreeSet<String>>>,
}

impl KnownDogs {
    /// Wraps a boot-time seed.
    pub(crate) fn new(names: BTreeSet<String>) -> Self {
        Self {
            names: Arc::new(Mutex::new(names)),
        }
    }

    /// Whether this shepherd may hold a section for `name`.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.lock().contains(name)
    }

    /// Records that `name` is a dog this shepherd knows about.
    pub(crate) fn insert(&self, name: &str) {
        self.lock().insert(name.to_owned());
    }

    /// A poisoned lock is recovered rather than propagated, as
    /// [`crate::dogs::DogRefusals`] does: these are names with no invariant
    /// a panic mid-write could have broken.
    fn lock(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.names.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything a request handler may touch; one clone per connection.
///
/// Every clone shares the same supervisor engine, event bus sender, flock
/// registry, and shutdown signal. The connection layer builds one from the
/// daemon's shared state and hands it to `dispatch` once per envelope.
///
/// Public because `tests/daemon_e2e.rs` holds one to drive [`Self::shutdown`]
/// and [`Self::snapshot_now`] without going through the socket. Its fields
/// are not.
#[derive(Clone, Debug)]
pub struct RpcContext {
    /// The supervisor engine this daemon is running.
    pub(crate) supervisor: SupervisorHandle,
    /// The daemon-wide event bus; `Subscribe` compiles a [`TopicFilter`] the
    /// connection layer hands to [`crate::bus::spawn_forwarder`] alongside a
    /// receiver off this sender.
    pub(crate) events: Bus,
    /// The muster roll's in-memory app registry; `Start` records into it.
    pub(crate) registry: FlockRegistry,
    /// Where [`Self::snapshot_now`] writes the muster roll.
    pub(crate) snapshot_path: PathBuf,
    /// Where `DogConfig` reads a dog's `[<name>]` section from: this home's
    /// `dogs.toml`, not `shep.toml`. The key carries no prefix.
    ///
    /// Re-read per request rather than held as parsed config, so `shep
    /// disable X && shep enable X` picks up an edited section
    /// (`crate::dogs::dog_section`).
    pub(crate) dogs_config: PathBuf,
    /// Every dog name this shepherd may hold a section for, running or
    /// not. See [`KnownDogs`].
    pub(crate) known_dogs: KnownDogs,
    /// Names from [`crate::boot::BootOptions::dogs`], the spawn list this
    /// daemon booted with, rather than [`Self::known_dogs`]' wider set of
    /// dogs that merely exist.
    ///
    /// Held rather than re-read from `shep.toml`, which this daemon never
    /// reads: a later boot plan (rebuilt at shutdown, or for a staged start)
    /// needs the same spawn list `boot` used, and this is where it survives
    /// between requests.
    pub(crate) dog_names: Vec<String>,
    /// Which of [`Self::dog_names`] run before every sheep rather than
    /// after the flock, from `[daemon] boot_first_dogs`.
    ///
    /// Held for the same reason as [`Self::dog_names`]: rebuilding the boot
    /// plan later needs to know which dogs were promoted, and this daemon
    /// has no other way to ask, since it never reads `shep.toml` itself. A
    /// name absent from [`Self::dog_names`] is inert here, not an error.
    pub(crate) boot_first_dogs: Vec<String>,
    /// This daemon's `$SHEP_HOME` layout, for assembling a dog's app config.
    pub(crate) paths: ShepPaths,
    /// This daemon's crate version, echoed in the handshake.
    pub(crate) daemon_version: String,
    /// Which dogs this daemon has refused at the handshake, and how often.
    ///
    /// Written and read by the connection layer's handshake, to decide
    /// whether a refused dog earns its one restart from disk or has already
    /// had it ([`crate::dogs::DogRefusals`]).
    pub(crate) dog_refusals: crate::dogs::DogRefusals,
    /// What has connected to this daemon's socket, by peer pid.
    ///
    /// Written by the connection layer, the one place that can see a peer's
    /// credentials, and read by [`crate::dogs::record_silent_dog`]. It tells
    /// a dog that never reached the socket apart from one that reached it and
    /// did not name itself: two silences with opposite fixes.
    pub(crate) peer_contacts: crate::dogs::PeerContacts,
    /// This daemon's OS pid, echoed in the handshake.
    pub(crate) pid: u32,
    /// Flips to `true` to start graceful daemon shutdown; see [`Self::shutdown`].
    pub(crate) shutdown: Arc<watch::Sender<bool>>,
    /// The live resource readings [`super::enrichment::with_live_stats`]
    /// takes a sample from.
    ///
    /// The same state the supervisor's extras hold: they decide which sheep
    /// is watched and record the periodic CPU baseline, and this side reads
    /// against it.
    pub(crate) stats: Arc<StatsState>,
    /// The machine's own numbers, read on [`crate::host`]'s tick.
    ///
    /// Held here for [`Self::stats`]' reason and read the same way: this
    /// side never samples, and the type is built so that it cannot.
    pub(crate) host: Arc<HostState>,
    /// What provider dogs have pushed, written by `PutSecrets` here and
    /// read per spawn by the supervisor actor. One registry, two owners,
    /// for the reason `stats` above gives.
    pub(crate) provider_secrets: Arc<ProviderSecrets>,
}

/// Where a muster roll landed and what it recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedRoll {
    /// The path written.
    pub path: PathBuf,
    /// How many apps the roll records.
    pub apps: u32,
}

impl RpcContext {
    /// Asks the daemon to begin graceful shutdown.
    ///
    /// Only flips the watch signal; the connection layer runs the kill
    /// ladder and closes listeners once it observes this go `true`.
    /// `dispatch` never calls it: `KillDaemon` reports the intent through
    /// `Outcome::Shutdown`, so the caller triggers it after the reply is on
    /// the wire.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Announces that these dogs' `dogs.toml` sections changed.
    ///
    /// The one place a `config.dog.<name>` frame comes from: the publisher
    /// has to be inside the daemon process, because that is where the bus
    /// is. The CLI's other two writers of `dogs.toml` say nothing.
    ///
    /// Public because the caller is `shep`'s own boot, which runs the
    /// migration before this daemon exists.
    pub fn announce_dog_config(&self, dogs: &[String]) {
        crate::bus::publish_dog_config_changed(&self.events, dogs);
    }

    /// Writes the muster roll now, reporting what it recorded.
    ///
    /// `None` means the supervisor engine has already stopped: there is
    /// nothing left to record and the shutdown path has already written the
    /// final roll.
    ///
    /// # Errors
    /// - [`SnapshotError`] as `write_atomic` reports it.
    pub async fn save_roll_now(&self) -> Result<Option<SavedRoll>, SnapshotError> {
        let Ok(infos) = self.supervisor.list_checked().await else {
            return Ok(None);
        };
        let roll = self.registry.roll(&infos, crate::now_ms());
        write_atomic(&self.snapshot_path, &roll)?;
        Ok(Some(SavedRoll {
            path: self.snapshot_path.clone(),
            // `u32` matches `SavedApp::instances_running`; a flock large
            // enough to overflow it has other problems.
            apps: u32::try_from(roll.apps.len()).unwrap_or(u32::MAX),
        }))
    }

    /// Writes the muster roll now, discarding what it recorded.
    ///
    /// # Errors
    /// - [`SnapshotError`] as `write_atomic` reports it.
    pub async fn snapshot_now(&self) -> Result<(), SnapshotError> {
        self.save_roll_now().await.map(|_| ())
    }
}

/// What the connection layer must do with a dispatched request.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Send this reply and keep reading.
    Reply(Reply),
    /// Send this reply, then start forwarding events through `filter`.
    Subscribe {
        /// The `Subscribed` (or error) reply to send first.
        reply: Reply,
        /// Compiled topic matcher for [`crate::bus::spawn_forwarder`].
        filter: TopicFilter,
    },
    /// Send this reply, then trigger daemon shutdown and close.
    Shutdown(Reply),
}

/// The deadline this envelope gets: its own, clamped, or the default.
#[must_use]
pub(crate) fn budget(deadline_ms: Option<u64>) -> Duration {
    // clamp's lower bound is 1ms so a literal `0` means "expire immediately"
    // rather than silently becoming "no deadline at all".
    Duration::from_millis(
        deadline_ms
            .unwrap_or(DEFAULT_DEADLINE_MS)
            .clamp(1, MAX_DEADLINE_MS),
    )
}
