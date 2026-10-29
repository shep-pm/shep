//! One sheep's row on the wire, its builder, and everything the row embeds.

use serde::{Deserialize, Serialize};

use crate::config::LevelRule;
use crate::status::ProcStatus;

use super::{DogAccess, DogSource, Vcs};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Response, SheepDrift, Smit};
#[cfg(doc)]
use crate::config::AppConfig;

/// One process the OS reports as a descendant of a sheep.
///
/// Not the set of processes that die with the sheep. This is a
/// parent-pid walk; the stop ladder acts on the process group. A
/// `setsid()` grandchild is in this list and outside the group.
///
/// `name` is the executable's name, never argv, which carries credentials.
// wire format: changing this is a breaking change
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lamb {
    /// The lamb's own pid.
    pub pid: u32,
    /// The executable's name, as the OS reports it. Never its command line.
    pub name: String,
    /// What the sheep named this lamb on its shepherd channel, with a
    /// [`ChildMessage::LambLabel`](crate::protocol::ChildMessage::LambLabel).
    ///
    /// `None` when it sent none, or the shepherd predates labels. Never
    /// read from the OS, so the no-argv rule above still holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Lamb {
    /// One lamb, with no label.
    #[must_use]
    pub fn new(pid: u32, name: impl Into<String>) -> Self {
        Self {
            pid,
            name: name.into(),
            label: None,
        }
    }

    /// This lamb, carrying the sheep's own label for it.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Why a sheep's process most recently stopped existing under this daemon.
///
/// Behind [`ProcessInfo::last_exit`]'s own `Option`, so `None` there means
/// never exited. Ordinarily exactly one of `code`/`signal` is `Some`,
/// mirroring the OS's `WIFEXITED`/`WIFSIGNALED` split; both `None` together
/// is legal and means this daemon recorded an exit it could not
/// characterize.
// wire format: changing this is a breaking change
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    /// The process's own exit code, set on a normal exit (`WIFEXITED`).
    pub code: Option<i32>,
    /// The raw unix signal number that ended the process, set when it did
    /// not exit on its own (`WIFSIGNALED`). An operator's own `shep stop`
    /// counts: the process still stopped by a signal.
    ///
    /// Platform-specific, and never rendered as a name here; that is an
    /// OS-aware layer's job.
    pub signal: Option<i32>,
}

/// Snapshot of one sheep for listings and events
///
/// Construct one with [`ProcessInfo::builder`]: `#[non_exhaustive]` forbids
/// a struct literal outside this crate, though not inside it. The fields
/// stay `pub`.
// wire format: changing this is a breaking change. No `Eq`: `cpu_percent`
// is an `f32`. Paths travel as `String`. serde's `PathBuf` refuses a
// non-UTF-8 path and would blank a whole `Reply`. Every added field is an
// `Option` or a defaulted collection: an older peer sends no key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// Stable numeric id
    pub id: u32,
    /// Sheep name
    pub name: String,
    /// Lifecycle status
    pub status: ProcStatus,
    /// OS pid while running
    pub pid: Option<u32>,
    /// Restart count since registration
    pub restarts: u32,
    /// Milliseconds since last successful start
    pub uptime_ms: u64,
    /// Fold membership
    pub fold: Option<String>,
    /// Names this sheep waits for at a staged start, from its
    /// `depends_on`. Empty both when the sheep declares none and when the
    /// peer daemon predates the field.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Resolved stdout log path: the app's explicit
    /// [`AppConfig::out_file`] when it set one, else the daemon-derived
    /// default. `None` only when the peer daemon predates this field.
    pub out_file: Option<String>,
    /// Resolved stderr log path, resolved exactly as [`Self::out_file`]
    pub err_file: Option<String>,
    /// Tree CPU as a percentage of one core, over the window since the
    /// daemon's last periodic sample. `None` when the sheep is not running,
    /// when it has been up for less than one sampling window, or when the
    /// peer daemon predates the field; all three render as unknown, never as
    /// zero. A value over 100 is a tree using more than one core.
    pub cpu_percent: Option<f32>,
    /// Tree resident set size in bytes, current as of the reply. `None`
    /// under the same three conditions as [`Self::cpu_percent`], minus the
    /// window one: memory needs no baseline.
    pub memory_bytes: Option<u64>,
    /// The tree's cumulative CPU-milliseconds, or `None` when the shepherd
    /// is not sampling this sheep.
    ///
    /// A sheep spawned since the last periodic tick has a counter but no
    /// baseline. So this is `Some` while [`Self::cpu_percent`] is `None`.
    ///
    /// A counter, not a rate: a client polling faster can difference two readings.
    pub cpu_ms: Option<u64>,
    /// Set when this entry is a dog, naming where the dog came from;
    /// `None` for a sheep.
    ///
    /// Two cases, not [`Self::cpu_percent`]'s three: "not a dog" is the true
    /// answer whether the peer predates the field or the entry is a sheep.
    pub dog: Option<DogSource>,
    /// The processes the OS reports as descendants of this sheep, or `None` when this reply did not walk for them.
    ///
    /// `None` covers two cases: this reply is not a `Describe` (only
    /// `Describe` walks), or the peer daemon predates the field.
    /// `Some(vec![])` is the third: walked, and this sheep has no children.
    ///
    /// Read [`Lamb`]'s own doc before rendering this: a stop kills a group, not this list.
    pub lambs: Option<Vec<Lamb>>,
    /// How this sheep's process most recently stopped existing under this
    /// daemon. `None` while it has never exited under this daemon, and when
    /// the peer daemon predates the field.
    ///
    /// Sticky across a respawn: it answers why the sheep last stopped, not
    /// whether it is stopped now, and updates only on the next exit.
    pub last_exit: Option<ExitInfo>,
    /// The last spawn refusal, including the process name, without environment values.
    /// Cleared when a spawn succeeds. Absent on older shepherds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_failure: Option<String>,
    /// The marker a dog has asked to have painted beside this sheep, or
    /// `None` when no dog has painted one, which also covers a peer daemon
    /// that predates the field.
    ///
    /// A `String` rather than a [`Smit`]: this is a report, and the daemon
    /// validated it at ingress. Every instance of a name shows the same marker.
    pub smit: Option<String>,
    /// Which instance slot of its app this sheep occupies, counting from 0.
    ///
    /// `None` when the peer daemon predates the field. Not a bare `u32`
    /// defaulted to 0: an app stocked to four instances would report four
    /// rows all claiming slot 0.
    pub instance: Option<u32>,
    /// Whether this dog has completed a handshake with the reporting
    /// shepherd, and not been refused since. `None` for a sheep.
    ///
    /// A sheep has no connection to the shepherd. `None` is the true answer
    /// for it and for a peer that predates the field.
    ///
    /// `Some(false)` covers a dog spawned a moment ago and one the shepherd refuses.
    pub handshook: Option<bool>,
    /// Whether the reporting shepherd has given up on this dog. It restarted
    /// it for never answering, watched that not help, and stopped restarting
    /// it. `None` for a sheep, on [`Self::handshook`]'s terms.
    ///
    /// Not derivable from [`Self::handshook`]. `Some(false)` there covers a
    /// dog that has not dialled back yet, and one given up on. It says the
    /// shepherd stopped, never why.
    pub dog_stale: Option<bool>,
    /// The [`AppConfig`] field names this sheep's spec differs from a load's
    /// parked config for, in field-name order. `None` when nothing is parked,
    /// and when the peer daemon predates the field.
    ///
    /// Names only, never values, as [`SheepDrift::fields`] carries them: a
    /// differing `env` reports `"env"` and stops there. `shep reload`
    /// promotes a parked config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Vec<String>>,
    /// The [`AppConfig`] field names an operator has set on this sheep that
    /// its current Flockfile does not declare, in field-name order. `None`
    /// when there is nothing to report, and when the peer daemon predates
    /// the field.
    ///
    /// Names only, never values, for [`Self::pending`]'s reason:
    /// [`crate::overrides::AppOverrides::fields`] can hold an `env` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overridden: Option<Vec<String>>,
    /// The sheep's `max_memory` ceiling in bytes, when it has one.
    ///
    /// Additive, like [`Self::instance`] and [`Self::handshook`] before it, so
    /// neither `PROTOCOL_VERSION` nor `SCHEMA_VERSION` moves: an older payload
    /// decodes with it absent and an older client ignores it. Lookout's
    /// `MEM/CEIL` gauge is the only reader; `None` draws an all-tail bar
    /// rather than guessing a denominator.
    pub max_memory: Option<u64>,
    /// How this sheep's own log lines announce their level, from its
    /// [`AppConfig::level_rules`](crate::config::AppConfig::level_rules).
    ///
    /// Empty both when the sheep declares none and when the peer daemon
    /// predates the field, which read the same way: a client classifying
    /// this sheep's lines falls back to its own reading of them. The key is
    /// absent from the payload entirely when the list is empty.
    // On the listing rather than behind a fetch of its own: a client reads
    // this on every line it draws, and a listing it already polls cannot go
    // stale between polls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub level_rules: Vec<LevelRule>,
    /// How long the shepherd gives one swap of this instance before it gives
    /// up: its `listen_timeout` plus `graceful_timeout` plus the shepherd's
    /// slack, in milliseconds.
    ///
    /// `Some` only on the rows of a reload's acceptance
    /// ([`Response::Reloading`]), and only where reload will replace. `None`
    /// elsewhere. Per instance, not per app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_deadline_ms: Option<u64>,
    /// `Some(true)` when the sheep's app is disabled: stopped, and refused a
    /// spawn until `shep enable`. `None` otherwise, which also covers a peer
    /// daemon that predates the field.
    ///
    /// Beside [`Self::status`] rather than a [`ProcStatus`] variant. A peer
    /// that predates a status could not tell it from `stopped`. A disabled
    /// row keeps `status: "stopped"`, and the key is never `Some(false)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Whether this sheep's own log lines carry the shepherd's stamp, from
    /// its [`AppConfig::log_timestamps`]. `Some(false)` is the only value a
    /// daemon sets. `None` for a dog, which always stamps, and for a peer
    /// daemon that predates the field: both read as stamped, which is what
    /// every daemon before it wrote. Additive, like [`Self::disabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_timestamps: Option<bool>,
    /// The marks on a dog's access: undeclared, not reviewed, permissions
    /// waiting. `None` for a sheep and in a reply from the shepherd, which
    /// does not know them; the CLI that prints a listing fills it in from
    /// `shep.toml`. Additive, like [`Self::disabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<DogAccess>,
    /// The revision this sheep's own directory is checked out on, read at
    /// report time. `Some` only on the rows of a `Describe`, like
    /// [`Self::lambs`]; `None` on every other row and for a peer daemon that
    /// predates the field.
    ///
    /// Additive, like [`Self::access`]: the key is absent until a shepherd has
    /// a revision to report, and no shepherd reports one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs: Option<Vcs>,
}

/// Orders one flock listing the way every operator-facing surface presents
/// one: `(name, instance, id)`.
///
/// Name first: an id is assigned at registration and renumbered by a
/// `delete all`, where a name survives. The id breaks the tie and stays
/// an addressing key (`shep stop 11`). A listing with `None` slots throughout collapses to `(name, id)`.
pub fn sort_flock(listing: &mut [ProcessInfo]) {
    listing.sort_unstable_by(|a, b| {
        (a.name.as_str(), a.instance, a.id).cmp(&(b.name.as_str(), b.instance, b.id))
    });
}

impl ProcessInfo {
    /// Starts a builder for one sheep's row.
    ///
    /// The three arguments are the fields no row can omit and no reader can
    /// default.
    ///
    /// No `#[must_use]`: [`ProcessInfoBuilder`] carries one, which clippy's
    /// `double_must_use` lint treats as covering this return too.
    pub fn builder(id: u32, name: impl Into<String>, status: ProcStatus) -> ProcessInfoBuilder {
        ProcessInfoBuilder {
            info: Self {
                id,
                name: name.into(),
                status,
                pid: None,
                restarts: 0,
                uptime_ms: 0,
                fold: None,
                depends_on: Vec::new(),
                out_file: None,
                err_file: None,
                cpu_percent: None,
                memory_bytes: None,
                cpu_ms: None,
                dog: None,
                lambs: None,
                last_exit: None,
                spawn_failure: None,
                smit: None,
                instance: None,
                handshook: None,
                dog_stale: None,
                pending: None,
                overridden: None,
                max_memory: None,
                level_rules: Vec::new(),
                reload_deadline_ms: None,
                disabled: None,
                log_timestamps: None,
                access: None,
                vcs: None,
            },
        }
    }
}

/// Builds a [`ProcessInfo`], which is `#[non_exhaustive]` and so cannot be
/// written as a struct literal outside this crate.
///
/// Every setter takes the field's own type, `Option` included. A caller
/// holding `Option<u32>` writes `.pid(entry.pid())` rather than an `if let`
/// ladder. A setter is skipped, not passed `None`, when a row has nothing
/// to say about that field.
#[derive(Debug, Clone)]
#[must_use = "a builder that is never `build`-ed produces no ProcessInfo"]
pub struct ProcessInfoBuilder {
    info: ProcessInfo,
}

impl ProcessInfoBuilder {
    /// Sets the OS pid; `None` while the sheep is not running.
    pub fn pid(mut self, pid: Option<u32>) -> Self {
        self.info.pid = pid;
        self
    }

    /// Sets the restart count since registration.
    pub fn restarts(mut self, restarts: u32) -> Self {
        self.info.restarts = restarts;
        self
    }

    /// Sets milliseconds since the last successful start.
    pub fn uptime_ms(mut self, uptime_ms: u64) -> Self {
        self.info.uptime_ms = uptime_ms;
        self
    }

    /// Sets fold membership.
    pub fn fold(mut self, fold: Option<String>) -> Self {
        self.info.fold = fold;
        self
    }

    /// Sets the names this sheep waits for at a staged start.
    pub fn depends_on(mut self, depends_on: Vec<String>) -> Self {
        self.info.depends_on = depends_on;
        self
    }

    /// Sets the resolved stdout log path.
    pub fn out_file(mut self, out_file: Option<String>) -> Self {
        self.info.out_file = out_file;
        self
    }

    /// Sets the resolved stderr log path.
    pub fn err_file(mut self, err_file: Option<String>) -> Self {
        self.info.err_file = err_file;
        self
    }

    /// Sets tree CPU as a percentage of one core.
    pub fn cpu_percent(mut self, cpu_percent: Option<f32>) -> Self {
        self.info.cpu_percent = cpu_percent;
        self
    }

    /// Sets tree resident set size in bytes.
    pub fn memory_bytes(mut self, memory_bytes: Option<u64>) -> Self {
        self.info.memory_bytes = memory_bytes;
        self
    }

    /// Sets the tree's cumulative CPU-milliseconds; `None` when unsampled.
    pub fn cpu_ms(mut self, cpu_ms: Option<u64>) -> Self {
        self.info.cpu_ms = cpu_ms;
        self
    }

    /// Marks this row a dog and names where the dog came from.
    pub fn dog(mut self, dog: Option<DogSource>) -> Self {
        self.info.dog = dog;
        self
    }

    /// Sets the sheep's lamb list; `None` when this reply did not walk for one.
    pub fn lambs(mut self, lambs: Option<Vec<Lamb>>) -> Self {
        self.info.lambs = lambs;
        self
    }

    /// Sets how this sheep's process most recently stopped; `None` while it
    /// has never exited under this daemon.
    pub fn last_exit(mut self, last_exit: Option<ExitInfo>) -> Self {
        self.info.last_exit = last_exit;
        self
    }

    /// Sets the last spawn refusal; `None` after a successful spawn.
    pub fn spawn_failure(mut self, spawn_failure: Option<String>) -> Self {
        self.info.spawn_failure = spawn_failure;
        self
    }

    /// Sets the marker a dog has painted on this sheep; `None` when none has.
    pub fn smit(mut self, smit: Option<String>) -> Self {
        self.info.smit = smit;
        self
    }

    /// Sets the instance slot; `None` when the peer daemon predates the field.
    pub fn instance(mut self, instance: Option<u32>) -> Self {
        self.info.instance = instance;
        self
    }

    /// Sets whether this dog has handshaken with the shepherd; `None` for a
    /// sheep, which has no handshake to report.
    pub fn handshook(mut self, handshook: Option<bool>) -> Self {
        self.info.handshook = handshook;
        self
    }

    /// Sets whether the shepherd has given up restarting this dog; `None`
    /// for a sheep, which is never given up on.
    pub fn dog_stale(mut self, dog_stale: Option<bool>) -> Self {
        self.info.dog_stale = dog_stale;
        self
    }

    /// Sets the field names a load has parked for this sheep's next spawn;
    /// `None` when nothing is parked.
    pub fn pending(mut self, pending: Option<Vec<String>>) -> Self {
        self.info.pending = pending;
        self
    }

    /// Sets the field names an operator has overridden on this sheep;
    /// `None` when there is nothing to report.
    pub fn overridden(mut self, overridden: Option<Vec<String>>) -> Self {
        self.info.overridden = overridden;
        self
    }

    /// Sets the sheep's `max_memory` ceiling in bytes; `None` when it has no
    /// ceiling configured.
    pub fn max_memory(mut self, max_memory: Option<u64>) -> Self {
        self.info.max_memory = max_memory;
        self
    }

    /// Sets the sheep's declared level rules; empty when it declares none.
    pub fn level_rules(mut self, level_rules: Vec<LevelRule>) -> Self {
        self.info.level_rules = level_rules;
        self
    }

    /// Sets one swap's own deadline in milliseconds; `None` on every row
    /// but a reload's, and on a row that reload will not replace.
    pub fn reload_deadline_ms(mut self, reload_deadline_ms: Option<u64>) -> Self {
        self.info.reload_deadline_ms = reload_deadline_ms;
        self
    }

    /// Sets whether the sheep's app is disabled; `None` when it is not.
    pub fn disabled(mut self, disabled: Option<bool>) -> Self {
        self.info.disabled = disabled;
        self
    }

    /// Sets whether the sheep's app turns the shepherd's stamp off; `None`
    /// for a dog, which always stamps.
    pub fn log_timestamps(mut self, log_timestamps: Option<bool>) -> Self {
        self.info.log_timestamps = log_timestamps;
        self
    }

    /// Sets the marks on this dog's access; `None` for a sheep.
    pub fn access(mut self, access: Option<DogAccess>) -> Self {
        self.info.access = access;
        self
    }

    /// Sets the revision this sheep's directory is on; `None` unless this row
    /// came from a `Describe`.
    pub fn vcs(mut self, vcs: Option<Vcs>) -> Self {
        self.info.vcs = vcs;
        self
    }

    /// Finishes the row.
    #[must_use]
    pub fn build(self) -> ProcessInfo {
        self.info
    }
}

#[cfg(test)]
pub(super) fn sample_info() -> ProcessInfo {
    ProcessInfo {
        id: 3,
        name: "web".to_string(),
        status: ProcStatus::Online,
        pid: Some(4242),
        restarts: 1,
        uptime_ms: 60_000,
        fold: Some("backend".to_string()),
        // Left empty: this fixture feeds `reply_wire_snapshots` and
        // `bus_event_wire_snapshots`, so a non-empty value moves pinned
        // bytes.
        depends_on: Vec::new(),
        out_file: Some("/home/ada/.shep/logs/web-0-out.log".to_string()),
        err_file: Some("/home/ada/.shep/logs/web-0-err.log".to_string()),
        // 12.5: an insta JSON snapshot is stable across platforms only
        // for a float the binary representation holds exactly.
        cpu_percent: Some(12.5),
        memory_bytes: Some(48 * 1024 * 1024),
        cpu_ms: None,
        dog: None,
        lambs: None,
        last_exit: Some(ExitInfo {
            code: Some(1),
            signal: None,
        }),
        spawn_failure: None,
        smit: None,
        instance: None,
        handshook: None,
        dog_stale: None,
        // Left at the builder's default: this fixture feeds
        // `reply_wire_snapshots` and `bus_event_wire_snapshots`, so a
        // `Some(..)` moves pinned bytes.
        pending: None,
        overridden: None,
        max_memory: Some(512 * 1024 * 1024),
        // Populated where its list-shaped neighbours above are not:
        // nothing else on the wire pins a `LevelRule`'s field names or a
        // `LineLevel` spelling, and an empty list would prove neither.
        level_rules: vec![crate::config::LevelRule {
            pattern: r"\[ERROR\]".to_string(),
            level: crate::config::LineLevel::Error,
        }],
        // `None` here so the key is absent from the pinned bytes; the
        // serialized shape of a real one is pinned on its own, by
        // `a_reload_deadline_is_absent_from_the_wire_until_a_reload_sets_one`.
        reload_deadline_ms: None,
        // `None` so the key is absent from the pinned bytes; its serialized
        // shape is pinned by `a_disabled_flag_is_absent_from_the_wire_until_set`.
        disabled: None,
        // `None` for the same reason, its shape pinned by
        // `a_stamps_off_flag_is_absent_from_the_wire_until_set`.
        log_timestamps: None,
        access: None,
        // `None` for the same reason, its shape pinned by
        // `a_row_with_no_revision_has_no_vcs_key` and
        // `a_revision_round_trips_with_and_without_a_branch`.
        vcs: None,
    }
}

/// A full 40-character SHA-1 hash, the shape [`Vcs::revision`] carries.
#[cfg(test)]
pub(super) const REVISION: &str = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";

#[cfg(test)]
mod sorting;
#[cfg(test)]
mod stamps;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vcs_wire;
