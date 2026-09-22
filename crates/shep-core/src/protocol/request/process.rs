//! One sheep's row on the wire, its builder, and everything the row embeds.

use serde::{Deserialize, Serialize};

use crate::config::LevelRule;
use crate::status::ProcStatus;

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Response, SheepDrift, Smit};
#[cfg(doc)]
use crate::config::AppConfig;

/// Where a dog came from: this binary, or one an operator adopted.
///
/// Carried on [`ProcessInfo::dog`], so a listing distinguishes the two
/// populations without a second request.
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DogSource {
    /// An argv branch of the shep binary itself (`shep dog <name>`).
    BuiltIn,
    /// A binary an operator adopted, run at the daemon's own trust level.
    Adopted {
        /// The binary's path, exactly as the operator gave it to `adopt`.
        path: String,
    },
}

impl DogSource {
    const fn as_str(&self) -> &'static str {
        match self {
            DogSource::BuiltIn => "built-in",
            DogSource::Adopted { .. } => "adopted",
        }
    }
}

impl From<&DogSource> for &'static str {
    fn from(source: &DogSource) -> Self {
        source.as_str()
    }
}

/// One process the OS reports as a descendant of a sheep.
///
/// Not the set of processes that die with the sheep: this is a parent-pid
/// walk, where the stop ladder acts on the process group. A lamb that forks
/// and exits leaves children re-parented to init, out of this list and still
/// in the group; a `setsid()` grandchild stays in the list and leaves the
/// group.
///
/// `name` is the executable's name (`node`, `sh`), never argv, which carries
/// credentials and would ride into `shep describe --format json`. Build one
/// with [`Self::new`].
// wire format: changing this is a breaking change
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lamb {
    /// The lamb's own pid.
    pub pid: u32,
    /// The executable's name, as the OS reports it. Never its command line.
    pub name: String,
}

impl Lamb {
    /// One lamb.
    #[must_use]
    pub fn new(pid: u32, name: impl Into<String>) -> Self {
        Self {
            pid,
            name: name.into(),
        }
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
// wire format: changing this is a breaking change. No `Eq`: `cpu_percent` is
// an `f32`. Paths travel as `String`, since serde's `PathBuf` refuses a
// non-UTF-8 path and would blank a whole `Reply`. Every added field is an
// `Option` or a defaulted collection, so a peer built before it sends no key
// and the empty reading means unknown.
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
    /// Present in one case [`Self::cpu_percent`] is not: a sheep spawned
    /// since the last periodic tick has a counter already, but no baseline
    /// to measure it against, so this is `Some` while the percent is still
    /// `None`.
    ///
    /// The counter rather than a rate, so a client polling faster than the
    /// shepherd's own sampling interval can difference two readings and get
    /// the mean over its own interval. [`Self::cpu_percent`] cannot serve
    /// that: it is measured against a baseline the shepherd rewrites on its
    /// own schedule, so consecutive readings share one and each is a running
    /// mean over a window that grows and then resets.
    pub cpu_ms: Option<u64>,
    /// Set when this entry is a dog, naming where the dog came from;
    /// `None` for a sheep.
    ///
    /// Two cases, not [`Self::cpu_percent`]'s three: "not a dog" is the true
    /// answer whether the peer predates the field or the entry is a sheep.
    pub dog: Option<DogSource>,
    /// The processes the OS reports as descendants of this sheep, or `None`
    /// when this reply did not walk for them.
    ///
    /// `None` covers two cases: this reply is not a `Describe` (only
    /// `Describe` walks), or the peer daemon predates the field.
    /// `Some(vec![])` is the third: walked, and this sheep has no children.
    ///
    /// Read [`Lamb`]'s own doc before rendering this: the list is not the set
    /// of processes a stop kills, and any output built from it has to say so.
    pub lambs: Option<Vec<Lamb>>,
    /// How this sheep's process most recently stopped existing under this
    /// daemon. `None` while it has never exited under this daemon, and when
    /// the peer daemon predates the field.
    ///
    /// Sticky across a respawn: it answers why the sheep last stopped, not
    /// whether it is stopped now, and updates only on the next exit.
    pub last_exit: Option<ExitInfo>,
    /// The marker a dog has asked to have painted beside this sheep, or
    /// `None` when no dog has painted one, which also covers a peer daemon
    /// that predates the field.
    ///
    /// A `String` rather than a [`Smit`]: this is a report, and the
    /// validation that makes it safe to print happened at the daemon's
    /// ingress. Every instance of a name shows the same marker, since smits
    /// are keyed by sheep name.
    pub smit: Option<String>,
    /// Which instance slot of its app this sheep occupies, counting from 0.
    ///
    /// `None` when the peer daemon predates the field. Not a bare `u32`
    /// defaulted to 0: an app stocked to four instances would report four
    /// rows all claiming slot 0.
    pub instance: Option<u32>,
    /// Whether this dog has completed a handshake with the shepherd that is
    /// reporting it, and not been refused since; `None` for a sheep.
    ///
    /// `None` on [`Self::dog`]'s two-case terms: a sheep has no connection
    /// to the shepherd, so "no handshake fact to report" is the true answer
    /// for a sheep and for a peer that predates the field alike.
    ///
    /// `Some(false)` is the one that matters: a dog on a protocol this
    /// shepherd refuses is alive, which is all [`Self::status`] reports, and
    /// not doing its job. A fact and not a verdict, though: a dog spawned a
    /// moment ago has not handshaken yet and is healthy.
    pub handshook: Option<bool>,
    /// Whether the reporting shepherd has given up on this dog: restarted it
    /// once for never answering, watched that not help, and stopped
    /// restarting it. `None` for a sheep, on [`Self::handshook`]'s terms.
    ///
    /// Not derivable from [`Self::handshook`]. `Some(false)` there covers
    /// both a dog spawned three seconds ago that has not dialled back and one
    /// this shepherd has permanently stopped restarting; the first needs
    /// nothing done and the second is an incident.
    ///
    /// A fact and not a verdict: it says the shepherd stopped, never why.
    /// The why is in that dog's own log (`shep bleats <dog>`).
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
    /// How long the shepherd gives ONE swap of this instance before it
    /// gives up on it: this instance's own `listen_timeout` plus its
    /// `graceful_timeout` plus the shepherd's slack, in milliseconds.
    ///
    /// `Some` only on the rows of a reload's acceptance
    /// ([`Response::Reloading`]), and only for an instance that reload will
    /// try to replace. `None` everywhere else, which covers a listing with
    /// no reload in flight, an instance a reload is skipping, and a peer
    /// daemon that predates the field.
    ///
    /// Per instance, not per app: a client that inferred it from its own
    /// copy of the Flockfile would be reading a file the shepherd may have
    /// moved past. Additive, like [`Self::max_memory`] before it, so
    /// neither `PROTOCOL_VERSION` nor `SCHEMA_VERSION` moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_deadline_ms: Option<u64>,
}

/// Orders one flock listing the way every operator-facing surface presents
/// one: `(name, instance, id)`.
///
/// Name first: an id is assigned at registration and a `delete all` plus a
/// fresh start renumbers the flock, where a name survives. A name is not a
/// total order on its own, so the id breaks the tie and stays an addressing
/// key (`shep stop 11`).
///
/// A listing whose rows all carry `None` for the slot collapses to
/// `(name, id)`, since `None` sorts before every `Some`.
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
                smit: None,
                instance: None,
                handshook: None,
                dog_stale: None,
                pending: None,
                overridden: None,
                max_memory: None,
                level_rules: Vec::new(),
                reload_deadline_ms: None,
            },
        }
    }
}

/// Builds a [`ProcessInfo`], which is `#[non_exhaustive]` and so cannot be
/// written as a struct literal outside this crate.
///
/// Every setter takes the field's own type, `Option` included, so a caller
/// already holding `Option<u32>` writes `.pid(entry.pid())` rather than an
/// `if let` ladder. A setter is skipped, not passed `None`, when a row has
/// nothing to say about that field; the skipped defaults are the ones a
/// not-yet-running sheep has.
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_builder_with_nothing_set_is_a_sheep_that_has_not_run() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Stopped).build();

        assert_eq!(info.id, 3);
        assert_eq!(info.name, "web");
        assert_eq!(info.status, ProcStatus::Stopped);
        assert_eq!(info.pid, None);
        assert_eq!(info.restarts, 0);
        assert_eq!(info.uptime_ms, 0);
        assert_eq!(info.fold, None);
        assert_eq!(info.out_file, None);
        assert_eq!(info.err_file, None);
        assert_eq!(info.cpu_percent, None);
        assert_eq!(info.memory_bytes, None);
        assert_eq!(info.cpu_ms, None);
        assert_eq!(info.dog, None);
        assert_eq!(info.lambs, None);
        assert_eq!(info.last_exit, None);
    }

    /// The raw counter rides the wire beside the percent. A client polling
    /// faster than the shepherd's own sampling interval differences two of
    /// these; `cpu_percent` cannot serve it, because consecutive readings
    /// share a baseline.
    #[test]
    fn a_process_info_carries_the_cpu_counter() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .cpu_ms(Some(1_234))
            .build();
        assert_eq!(info.cpu_ms, Some(1_234));
    }

    /// Absent by default, like every other sampled field: a lifecycle verb's
    /// answer carries no reading.
    #[test]
    fn a_process_info_without_a_reading_has_no_counter() {
        let info = ProcessInfo::builder(3, "web", ProcStatus::Online).build();
        assert_eq!(info.cpu_ms, None);
    }

    /// Every field is given a value distinct from every other field's
    /// default, so a copy-pasted setter body shows up as a mismatch.
    #[test]
    fn every_setter_writes_its_own_field_and_no_other() {
        let built = ProcessInfo::builder(3, "web", ProcStatus::Online)
            .pid(Some(4242))
            .restarts(1)
            .uptime_ms(60_000)
            .fold(Some("backend".to_string()))
            .out_file(Some("/home/ada/.shep/logs/web-0-out.log".to_string()))
            .err_file(Some("/home/ada/.shep/logs/web-0-err.log".to_string()))
            .cpu_percent(Some(12.5))
            .memory_bytes(Some(48 * 1024 * 1024))
            .dog(None)
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .max_memory(Some(512 * 1024 * 1024))
            .level_rules(vec![crate::config::LevelRule {
                pattern: r"\[ERROR\]".to_string(),
                level: crate::config::LineLevel::Error,
            }])
            .build();

        // `sample_info()` is a struct literal on purpose: it is the one
        // place that names every field by hand, so this comparison fails the
        // day the struct grows a field the builder cannot set.
        assert_eq!(built, sample_info());

        // `sample_info()`'s `dog` is `None`, the builder's default too, so an
        // empty `dog` setter body would pass the comparison above. It cannot
        // be changed: it feeds pinned snapshots.
        assert_eq!(
            ProcessInfo::builder(1, "metrics", ProcStatus::Online)
                .dog(Some(DogSource::BuiltIn))
                .build()
                .dog,
            Some(DogSource::BuiltIn),
            "an empty `dog` setter body is invisible to the comparison above"
        );

        // `lambs`, on `dog`'s terms above.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .lambs(Some(vec![Lamb::new(4243, "node")]))
                .build()
                .lambs,
            Some(vec![Lamb::new(4243, "node")]),
            "an empty `lambs` setter body is invisible to the comparison above"
        );

        // `smit`, on the same terms, and the field a third party writes: an
        // empty setter body drops every dog's mark.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .smit(Some("\u{25b2} main@a1b2c3".to_string()))
                .build()
                .smit
                .as_deref(),
            Some("\u{25b2} main@a1b2c3"),
            "an empty `smit` setter body is invisible to the comparison above"
        );

        // `handshook`, on the same terms.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .handshook(Some(false))
                .build()
                .handshook,
            Some(false),
            "an empty `handshook` setter body is invisible to the comparison above"
        );

        // `dog_stale`, paired with `handshook`: both default to `None`.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .dog_stale(Some(true))
                .build()
                .dog_stale,
            Some(true),
            "an empty `dog_stale` setter body is invisible to the comparison above"
        );

        // `pending`, on the same terms.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .pending(Some(vec!["env".to_string()]))
                .build()
                .pending,
            Some(vec!["env".to_string()]),
            "an empty `pending` setter body is invisible to the comparison above"
        );

        // `overridden`, on the same terms.
        assert_eq!(
            ProcessInfo::builder(1, "web", ProcStatus::Online)
                .overridden(Some(vec!["cwd".to_string()]))
                .build()
                .overridden,
            Some(vec!["cwd".to_string()]),
            "an empty `overridden` setter body is invisible to the comparison above"
        );
    }

    #[test]
    fn lambs_distinguishes_not_walked_from_walked_and_empty() {
        let not_walked = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        assert_eq!(not_walked.lambs, None);

        let walked_empty = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(Vec::new()))
            .build();
        assert_eq!(walked_empty.lambs, Some(Vec::new()));
    }

    #[test]
    fn a_process_info_without_a_lambs_key_still_deserializes() {
        let fixture = r#"{
            "id": 3, "name": "web", "status": "online", "pid": 4242,
            "restarts": 0, "uptime_ms": 100, "fold": null,
            "out_file": null, "err_file": null,
            "cpu_percent": null, "memory_bytes": null, "dog": null
        }"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.lambs, None);
    }

    /// argv holds credentials (`--password=`, `?token=`) and
    /// `shep describe --format json` is output people paste into issues.
    #[test]
    fn a_lamb_is_a_pid_and_an_executable_name() {
        let lamb = Lamb::new(4243, "node");
        let json = serde_json::to_string(&lamb).unwrap();
        assert_eq!(json, r#"{"pid":4243,"name":"node"}"#);
        assert_eq!(serde_json::from_str::<Lamb>(&json).unwrap(), lamb);
    }

    #[test]
    fn a_dog_source_serializes_snake_case_under_its_kind() {
        assert_eq!(
            serde_json::to_string(&DogSource::BuiltIn).unwrap(),
            r#"{"kind":"built_in"}"#
        );
        let adopted = DogSource::Adopted {
            path: "/usr/local/bin/shep-otel".to_string(),
        };
        let wire = r#"{"kind":"adopted","path":"/usr/local/bin/shep-otel"}"#;
        assert_eq!(serde_json::to_string(&adopted).unwrap(), wire);
        assert_eq!(serde_json::from_str::<DogSource>(wire).unwrap(), adopted);
    }

    #[test]
    fn v1_process_info_without_a_dog_marker_still_deserializes() {
        let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log","cpu_percent":12.5,"memory_bytes":50331648}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.dog, None);
    }

    /// No field here carries `#[serde(default)]`: serde's derive resolves a
    /// missing key to `None` for a field whose type is syntactically
    /// `Option<...>`.
    #[test]
    fn a_process_info_without_a_last_exit_key_still_deserializes() {
        let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log","cpu_percent":12.5,"memory_bytes":50331648,"dog":null,"lambs":null}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.last_exit, None);
    }

    #[test]
    fn a_process_info_without_a_smit_key_still_deserializes() {
        let fixture = r#"{"id":1,"name":"web","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":null,"lambs":null,"last_exit":null}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.smit, None);
    }

    /// The fixture is a dog's row, where `None` means "render this as it
    /// rendered before the field existed", never "never handshaken".
    #[test]
    fn a_process_info_without_a_handshook_key_still_deserializes() {
        let fixture = r#"{"id":1,"name":"metrics","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":{"kind":"built_in"},"lambs":null,"last_exit":null,"smit":null,"instance":0}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.handshook, None);
        assert_eq!(info.dog, Some(DogSource::BuiltIn));
    }

    /// The fixture carries `handshook: false`, the case that matters: `None`
    /// is "no verdict to report", never "it has not given up".
    #[test]
    fn a_process_info_without_a_dog_stale_key_still_deserializes() {
        let fixture = r#"{"id":1,"name":"metrics","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":{"kind":"built_in"},"lambs":null,"last_exit":null,"smit":null,"instance":0,"handshook":false}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.dog_stale, None);
        assert_eq!(info.handshook, Some(false));
    }

    #[test]
    fn a_process_info_carries_its_memory_ceiling_and_defaults_to_none() {
        let plain = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        assert_eq!(
            plain.max_memory, None,
            "a sheep with no ceiling reports none"
        );

        let capped = ProcessInfo::builder(2, "hungry", ProcStatus::Online)
            .max_memory(Some(52 * 1024 * 1024))
            .build();
        assert_eq!(capped.max_memory, Some(54_525_952));
    }

    /// Absent rather than `null` when there is no reload, so a listing's
    /// payload is the shape it always was and `SCHEMA_VERSION` stays put.
    /// Present as a plain number when a reload sets one, which is the only
    /// place a client reads it.
    #[test]
    fn a_reload_deadline_is_absent_from_the_wire_until_a_reload_sets_one() {
        let quiet = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        let json = serde_json::to_string(&quiet).expect("serialize");
        assert!(
            !json.contains("reload_deadline_ms"),
            "a row with no reload carries no key at all: {json}"
        );

        let reloading = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .reload_deadline_ms(Some(16_000))
            .build();
        let json = serde_json::to_string(&reloading).expect("serialize");
        assert!(
            json.contains(r#""reload_deadline_ms":16000"#),
            "a reload's own row carries the number: {json}"
        );
        let back: ProcessInfo = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.reload_deadline_ms, Some(16_000));
    }

    #[test]
    fn an_older_daemons_process_info_still_decodes() {
        // Every additive field, asserted off one payload. A payload written
        // before any of them existed has to decode with each absent rather
        // than fail the whole envelope, and one fixture cannot drift from
        // another.
        let older = r#"{"id":1,"name":"web","status":"online","restarts":0,"uptime_ms":0}"#;
        let info: ProcessInfo = serde_json::from_str(older).expect("an older payload decodes");
        assert_eq!(info.max_memory, None);
        assert_eq!(info.reload_deadline_ms, None);
    }

    #[test]
    fn v1_process_info_without_stats_still_deserializes() {
        let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log"}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.cpu_percent, None);
        assert_eq!(info.memory_bytes, None);
    }

    #[test]
    fn v1_process_info_without_log_paths_still_deserializes() {
        // Committed byte fixture from before `out_file`/`err_file` existed.
        let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend"}"#;
        let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
        assert_eq!(info.id, 3);
        assert_eq!(info.out_file, None);
        assert_eq!(info.err_file, None);
    }

    #[test]
    fn an_old_client_still_decodes_a_new_process_info() {
        // `ProcessInfo` carries no `deny_unknown_fields`, unlike the config
        // types in `crate::config`, so extra keys are ignored.
        #[derive(Deserialize)]
        struct V1ProcessInfo {
            id: u32,
            fold: Option<String>,
        }

        let current = serde_json::to_string(&sample_info()).unwrap();
        let old: V1ProcessInfo = serde_json::from_str(&current).unwrap();
        assert_eq!(old.id, 3);
        assert_eq!(old.fold.as_deref(), Some("backend"));
    }

    /// The fixture cannot agree under either candidate order: by id it is
    /// `web/1, api/2, web/0`, by name `api, web, web`. The two `web` rows
    /// are the tiebreak half, seeded out of order.
    #[test]
    fn a_listing_sorts_by_name_then_by_id() {
        let mut listing = vec![
            ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
            ProcessInfo::builder(2, "api", ProcStatus::Online).build(),
            ProcessInfo::builder(0, "web", ProcStatus::Online).build(),
        ];
        sort_flock(&mut listing);

        let seen: Vec<(&str, u32)> = listing
            .iter()
            .map(|info| (info.name.as_str(), info.id))
            .collect();
        assert_eq!(
            seen,
            vec![("api", 2), ("web", 0), ("web", 1)],
            "name first, then id inside a name"
        );
    }

    #[test]
    fn an_instance_slot_survives_a_round_trip_and_defaults_to_absent() {
        let with = ProcessInfo::builder(1, "web", ProcStatus::Online)
            .instance(Some(2))
            .build();
        assert_eq!(with.instance, Some(2));

        let without = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
        assert_eq!(
            without.instance, None,
            "a row nobody set a slot on says so, rather than claiming slot 0"
        );
    }

    #[test]
    fn a_reply_from_a_daemon_without_the_field_deserializes_as_absent() {
        let json = r#"{"id":1,"name":"web","status":"online","pid":null,
            "restarts":0,"uptime_ms":0,"fold":null,"out_file":null,
            "err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":null,
            "lambs":null,"last_exit":null,"smit":null}"#;
        let info: ProcessInfo = serde_json::from_str(json).expect("older reply still parses");
        assert_eq!(info.instance, None);
    }

    #[test]
    fn sort_flock_orders_by_slot_before_id() {
        // A reload gave slot 0 a fresh, higher id. Slot order must still win.
        let mut listing = vec![
            ProcessInfo::builder(9, "web", ProcStatus::Online)
                .instance(Some(0))
                .build(),
            ProcessInfo::builder(2, "web", ProcStatus::Online)
                .instance(Some(1))
                .build(),
        ];
        sort_flock(&mut listing);
        assert_eq!(
            listing.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![9, 2],
            "slot 0 leads even though its id is higher"
        );
    }

    #[test]
    fn sort_flock_falls_back_to_id_when_no_row_carries_a_slot() {
        let mut listing = vec![
            ProcessInfo::builder(5, "web", ProcStatus::Online).build(),
            ProcessInfo::builder(3, "web", ProcStatus::Online).build(),
        ];
        sort_flock(&mut listing);
        assert_eq!(
            listing.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![3, 5],
            "an older daemon's listing sorts exactly as it does today"
        );
    }
}
