//! Per-connection state: [`RpcContext`], its deadline budget, and the
//! [`Outcome`] a handler hands back to the connection layer.
//!
//! [`KnownDogs`] is the set of dog names this shepherd may configure;
//! [`SavedRoll`] is what [`RpcContext::save_roll_now`] reports having
//! written.

use core::time::Duration;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
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

/// Every dog name this shepherd may hold a section for, running or not,
/// and what each was granted.
///
/// Seeded at boot from the CLI, which owns `shep.toml`. A
/// `Request::EnableDog` adds a name and records the grants it carries, so a
/// dog adopted against a running shepherd needs no reload. Never shrunk:
/// `shep disable` leaves the section in `dogs.toml`, where a disabled dog still wants configuring.
#[derive(Debug, Clone)]
pub(crate) struct KnownDogs {
    dogs: Arc<Mutex<BTreeMap<String, Option<Grants>>>>,
}

impl KnownDogs {
    /// Wraps a boot-time seed of undeclared dogs.
    #[cfg(test)]
    pub(crate) fn new(names: std::collections::BTreeSet<String>) -> Self {
        Self::with_grants(names.into_iter().map(|name| (name, None)))
    }

    /// Wraps a boot-time seed, each name with the grants it was spawned
    /// with.
    pub(crate) fn with_grants(dogs: impl IntoIterator<Item = (String, Option<Grants>)>) -> Self {
        Self {
            dogs: Arc::new(Mutex::new(dogs.into_iter().collect())),
        }
    }

    /// Whether this shepherd may hold a section for `name`.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.lock().contains_key(name)
    }

    /// Records that `name` is a dog this shepherd knows about, with the
    /// `grants` it now holds. `None` leaves it undeclared.
    #[cfg(test)]
    pub(crate) fn insert(&self, name: &str, grants: Option<Grants>) {
        self.lock().insert(name.to_owned(), grants);
    }

    /// Records `grants` for `name` and hands back what it held before, for
    /// [`Self::restore`] when the start it was recorded ahead of is refused.
    pub(crate) fn replace(&self, name: &str, grants: Option<Grants>) -> Held {
        Held(self.lock().insert(name.to_owned(), grants))
    }

    /// Puts `name` back as [`Self::replace`] found it.
    pub(crate) fn restore(&self, name: &str, held: Held) {
        match held.0 {
            Some(grants) => self.lock().insert(name.to_owned(), grants),
            None => self.lock().remove(name),
        };
    }

    /// What is recorded for `name`: `None` for a name never seen, and
    /// otherwise its grants, themselves `None` when it is undeclared.
    pub(crate) fn recorded(&self, name: &str) -> Option<Option<Grants>> {
        self.lock().get(name).cloned()
    }

    /// What `name` was granted, or `None` when it is undeclared or unknown.
    pub(crate) fn grants(&self, name: &str) -> Option<Grants> {
        self.lock().get(name).cloned().flatten()
    }

    /// A poisoned lock is recovered rather than propagated, as
    /// [`crate::dogs::DogRefusals`] does: these are names with no invariant
    /// a panic mid-write could have broken.
    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Option<Grants>>> {
        self.dogs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What [`KnownDogs`] held for a name before [`KnownDogs::replace`]: nothing
/// at all, or an entry that was undeclared or granted.
#[derive(Debug)]
pub(crate) struct Held(Option<Option<Grants>>);

/// Everything a request handler may touch; one clone per connection.
///
/// Every clone shares the same supervisor engine, event bus sender, flock
/// registry, and shutdown signal. The connection layer builds one from the
/// daemon's shared state and hands it to `dispatch` once per envelope.
///
/// Public because `tests/daemon_e2e.rs` holds one. Its fields are not.
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
    /// reads: a later boot plan needs the same spawn list `boot` used.
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
    /// credentials, and read by `dogs::silent::record_silent_dog`. It tells
    /// a dog that never reached the socket apart from one that reached it and
    /// did not name itself: two silences with opposite fixes.
    pub(crate) peer_contacts: crate::dogs::PeerContacts,
    /// This daemon's OS pid, echoed in the handshake.
    pub(crate) pid: u32,
    /// Flips to `true` to start graceful daemon shutdown; see [`Self::shutdown`].
    pub(crate) shutdown: watch::Sender<bool>,
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
    /// The environment a dog's `{{secret:...}}` references resolve in, from
    /// `[daemon] environment`.
    ///
    /// The supervisor holds the same name for the sheep it spawns. A dog
    /// has no environment of its own to override it with, so this is the
    /// only one `Request::DogConfigResolved` ever reads in.
    pub(crate) host_environment: String,
    /// What delivers a signal to a lamb, which the supervisor does not own.
    ///
    /// The operating system's in a real shepherd, a recorder in a test.
    pub(crate) pid_signals: Arc<dyn super::PidSignals>,
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
    /// Public because the caller is `shep`'s own boot, which runs the migration before this daemon exists.
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
