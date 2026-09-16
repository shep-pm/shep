//! [`Response`], one variant per request kind, and the host reading one of them carries.

use serde::{Deserialize, Serialize};

use super::{
    ActionReply, DogSectionToml, LineReply, ProcessInfo, SheepApplied, SheepConfigView, SheepDrift,
    SheepRefusal, SignalReply,
};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Request, sort_flock};
#[cfg(doc)]
use crate::config::AppConfig;

/// The machine the flock runs on, as the shepherd last read it.
///
/// Three of the four numbers are rates, so none of them exists in a single
/// moment: CPU, disk traffic and network traffic are all differences between
/// two readings of a counter the kernel only ever increments. The shepherd
/// keeps the earlier reading between its own ticks and serves the
/// difference, which is what lets a one-shot listing carry them at all.
///
/// Answered by [`Response::HostUsage`], and by nothing else: this is
/// deliberately not a field on [`Response::Flock`], whose array shape
/// predates it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HostUsage {
    /// Every core's usage over the shepherd's last window, as one percentage
    /// of one whole machine.
    ///
    /// `None` where no window long enough to divide by has passed yet, which
    /// on a shepherd that has just booted it has not. Absent is not idle.
    pub cpu_percent: Option<f32>,
    /// Memory in use, as the platform reports it. Not a rate, so never
    /// absent.
    pub memory_used_bytes: u64,
    /// Total physical memory.
    pub memory_total_bytes: u64,
    /// Bytes a second the block devices read and wrote over that window,
    /// absent on [`Self::cpu_percent`]'s terms.
    pub disk_bytes_per_second: Option<(u64, u64)>,
    /// Bytes a second the non-loopback interfaces received and transmitted
    /// over that window, absent on the same terms.
    pub network_bytes_per_second: Option<(u64, u64)>,
}

/// One RPC response (pairs with [`Request`] variants)
///
/// Ten variants carry a bare `Vec<ProcessInfo>`. Do not collapse them into
/// one: each names which request it answers, which is what lets a variant
/// diverge without a protocol bump. `Reloading` already means an acceptance
/// rather than a result, `Scaled` only the survivors of a scale-down, and
/// `Mustered` every sheep of every restored app rather than what this call
/// started.
// wire format: changing existing variants is a breaking change.
// `large_enum_variant` allowed, not fixed: clippy's remedy is to box
// `DogStarted`'s payload, a source break for every
// `Response::DogStarted(info)` in and out of this workspace.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    /// Answer to `Ping`
    Pong,
    /// Answer to `ListFlock`
    Flock(Vec<ProcessInfo>),
    /// Answer to `HostUsage`
    ///
    /// `None` on a platform `sysinfo` cannot read at all, which is a state a
    /// caller renders rather than an error. A supported platform with no
    /// window yet answers `Some`, carrying the memory it could read and
    /// `None` for each rate it could not.
    HostUsage(Option<HostUsage>),
    /// Answer to `Describe`
    Described(Vec<ProcessInfo>),
    /// Answer to `Start`
    Started(Vec<ProcessInfo>),
    /// Answer to `Add`: one row per app the request named, registered and
    /// spawning nothing.
    ///
    /// A row here can still be `Online`: `Add` is idempotent by name, so the
    /// reply describes the membership the request leaves behind.
    Added(Vec<ProcessInfo>),
    /// Answer to `ConfigDrift`: one entry per app that is registered under a
    /// config different from the one asked about, and no entry for anything
    /// else. An empty vector means every app asked about either matches or
    /// is not registered at all.
    Drifted(Vec<SheepDrift>),
    /// Answer to `ApplyConfig`: one entry per app the request named, in the
    /// order it named them, the refused and the unchanged included.
    ///
    /// Complete where [`Self::Drifted`] is filtered: an app missing from
    /// "what did you do to each of these" looks like one the daemon dropped.
    Applied(Vec<SheepApplied>),
    /// Answer to `SheepConfig`: one sheep's config with `env` emptied and
    /// its keys listed beside it.
    ///
    /// Boxed, and the only variant here that is. This one carries a whole
    /// [`AppConfig`], which is several times the size of anything else in
    /// the enum, and a `Response` is inside a `Reply` which is inside a
    /// [`ServerFrame`](crate::protocol::ServerFrame): without the box,
    /// every frame the daemon sends costs the largest config's worth of
    /// stack for a variant almost none of them use.
    ///
    /// The enum-level `#[allow(clippy::large_enum_variant)]` below does not
    /// cover it, and the difference is the point of that allow's own
    /// argument: boxing `DogStarted` would be a source break for every
    /// `Response::DogStarted(info)` in and out of this workspace, where
    /// this variant has never shipped and so breaks nobody.
    ///
    /// `Box<T>` serializes exactly as `T`, so the wire bytes and the pinned
    /// fixtures are untouched.
    SheepConfig(Box<SheepConfigView>),
    /// Answer to `SetSheepEnv`: the key that was set or removed.
    ///
    /// Never the value, and never the resulting env map. This reply exists
    /// to confirm which key moved, and echoing what was just written back
    /// down a socket would undo the whole point of `SheepConfig` withholding
    /// it (IR-41).
    SheepEnvSet {
        /// The sheep.
        name: String,
        /// The key.
        key: String,
    },
    /// What a [`Request::SetSheepEnvBatch`] did, or would have done.
    ///
    /// Key names only. `set` is what was written, `unchanged` what already
    /// held the same value, `collisions` what held a different one. A
    /// forced request reports a collision in both `set` and `collisions`;
    /// an unforced one that collides reports an empty `set` and wrote
    /// nothing.
    SheepEnvBatch {
        /// The sheep's name.
        name: String,
        /// Keys written.
        set: Vec<String>,
        /// Keys that already held this value.
        unchanged: Vec<String>,
        /// Keys that held a different value.
        collisions: Vec<String>,
    },
    /// Answer to `SetSheepField`: which field moved, and whether the
    /// running child has it.
    ///
    /// Not [`Self::Applied`]'s three lists; the difference is the
    /// request's own shape. `applied`, `pending` and `refused` exist
    /// because `ApplyConfig` carries N apps of M fields, so a caller cannot
    /// otherwise tell which field went where or that one app of eleven was
    /// refused. This request carries one field of one sheep, so `refused`
    /// would be a second way to say no beside the `Err` arm (a client
    /// checking only the `Err` would silently swallow the other), and the
    /// two lists collapse to the one bit that is left.
    ///
    /// That bit is not redundant with the field's own
    /// [`ApplyGroup`](crate::config::ApplyGroup), which the caller already
    /// knows. It is the daemon's answer about state a caller cannot see:
    /// `autostart` is `NextSpawn` and yet reports as in force, because it
    /// is read at muster rather than at a spawn, and a `Live` field whose
    /// config subset will not normalize on its own parks instead of
    /// applying.
    SheepFieldSet {
        /// The sheep.
        name: String,
        /// The field that moved.
        key: String,
        /// `true` when the running child does not have the value yet and
        /// `shep reload <name>` is what promotes it. A client rendering
        /// this says so, the same rule [`SheepApplied::pending`] carries.
        pending: bool,
        /// A `cwd`, `script`, `out_file` or `err_file` that looks wrong on
        /// disk, `None` for every other field and for one of these four
        /// that looks fine.
        ///
        /// Advisory, not a second way to say no: the write above still
        /// landed. `normalize` cannot see the filesystem (a daemon and a
        /// CLI normalizing the same config may run as different users), so
        /// this is checked once, daemon-side, after the value is already
        /// accepted, and a directory a deploy script has not created yet is
        /// a real config the write must not refuse. The window between this
        /// check and the respawn that actually needs the path is the same
        /// one `check_log_ancestry`'s own doc comment names for its check
        /// (`docs/specs/deferred.md`).
        ///
        /// Additive: absent rather than `null` on a peer built before this
        /// field existed, so `PROTOCOL_VERSION` does not move for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    /// Answer to `SetDogConfig`: the section was written and the topic
    /// published.
    DogConfigSet {
        /// The dog.
        name: String,
    },
    /// Answer to [`Request::PutSecrets`]: how many entries were stored.
    SecretsPut {
        /// Entry count, after the namespace and environment were replaced.
        accepted: u32,
    },
    /// Answer to `Stop`
    Stopped(Vec<ProcessInfo>),
    /// Answer to `Restart`: the sheep that were restarted, one row each.
    ///
    /// **When the reply arrives depends on how many sheep matched.** One
    /// sheep is answered as soon as its respawn is issued. Two or more are
    /// restarted in dependency order, and the daemon holds each stage until
    /// the apps a later stage waits on are back, so the reply arrives no
    /// sooner than the last stage's respawns and the rows are stitched from
    /// one answer per stage. A client asking for a budget sizes it for the
    /// whole walk, not for one respawn.
    Restarted {
        /// The sheep the restart reached, one row each.
        ///
        /// A row is not a promise the process is up. A respawn that could
        /// not exec is an `errored` row here rather than an entry in
        /// `refused` below: the sheep was reached and the restart was not
        /// refused, it is the child that failed.
        accepted: Vec<ProcessInfo>,
        /// The apps the walk could not restart, empty when it restarted
        /// every one it named.
        ///
        /// Only a walk fills this. A selector matching one app is refused
        /// whole, as the `Err` arm, so a client reading a single-target
        /// restart never sees a row here.
        refused: Vec<SheepRefusal>,
    },
    /// Answer to `Reload`: acceptances, not results.
    ///
    /// One instance costs a readiness wait plus a drain, so a clustered app
    /// outlasts any deadline a client may ask for. Every row is therefore the
    /// sheep as it stood when its own reload was accepted, and the swaps
    /// report themselves on the bus (`process.reload`, `process.reloaded`,
    /// `process.reload_abandoned`). A matched sheep with nothing to replace
    /// is listed as the no-op success it is.
    ///
    /// **When the reply arrives depends on how many sheep matched.** One
    /// sheep is answered as soon as its reload is accepted. Two or more are
    /// reloaded in dependency order, and the daemon holds each stage until
    /// the swaps of the apps a later stage waits on have landed, so the
    /// reply arrives no sooner than the last stage's acceptance and the rows
    /// are stitched from one acceptance per stage. A client asking for a
    /// budget sizes it for the whole walk, not for one acceptance.
    Reloading {
        /// The sheep whose reloads were accepted, one row each.
        accepted: Vec<ProcessInfo>,
        /// The apps the walk could not reload, empty when it reloaded every
        /// one it named.
        ///
        /// Only a walk fills this. A selector matching one app is refused
        /// whole, as the `Err` arm, so a client reading a single-target
        /// reload never sees a row here.
        refused: Vec<SheepRefusal>,
    },
    /// Answer to `Scale`: the app's instances that will remain, one row each,
    /// ordered by [`sort_flock`]. Every row shares one name, so that is slot
    /// order with the id breaking a tie.
    ///
    /// Scaling down, the departing instances are absent even though their
    /// kill ladders are still running; they report themselves on the bus as
    /// `process.delete`.
    Scaled(Vec<ProcessInfo>),
    /// Answer to `SetSmit`: every instance of the named sheep, one row each,
    /// carrying the smit as it now stands.
    SmitPainted(Vec<ProcessInfo>),
    /// Answer to `Delete`: ids removed
    Deleted(Vec<u32>),
    /// Answer to `Reopen`: every matched sheep, running or not. A sheep with
    /// no live log pump has nothing to reopen and is reported as a success,
    /// so this carries the same matches `Describe` would.
    Reopened(Vec<ProcessInfo>),
    /// Answer to `Flush`: one row per matched sheep, running or not, exactly
    /// as [`Self::Reopened`].
    ///
    /// One row per sheep, not per file emptied: several sheep can share one
    /// log path, and the daemon truncates each distinct path once.
    Flushed(Vec<ProcessInfo>),
    /// Answer to `Trigger`: one [`ActionReply`] row per matched sheep, rather
    /// than a flock listing, since `ProcessInfo` has nowhere to hold a reply
    /// body.
    Triggered(Vec<ActionReply>),
    /// Answer to `Signal`: one [`SignalReply`] row per matched sheep.
    ///
    /// Not a flock listing: [`ProcessInfo`] has nowhere to hold a per-sheep
    /// outcome.
    Signalled(Vec<SignalReply>),
    /// Answer to `SendLine`: one [`LineReply`] row per matched sheep.
    SentLine(Vec<LineReply>),
    /// Answer to `SaveRoll`
    RollSaved {
        /// Absolute path of the roll the daemon wrote
        path: String,
        /// How many apps that roll records
        apps: u32,
    },
    /// Answer to `Muster`: every sheep of every app the roll restored, not
    /// only the ones this call spawned.
    ///
    /// Assembling a flock that is already assembled starts nothing, so a
    /// listing of what this call spawned would be indistinguishable from an
    /// empty roll.
    Mustered(Vec<ProcessInfo>),
    /// Answer to `DogConfig`: the dog's own section, rendered back to TOML.
    ///
    /// `toml` is [`DogSectionToml`], whose manual `Debug` keeps the webhook
    /// credentials this text carries out of a `{:?}`-formatted `Response`.
    DogSection {
        /// The `[dog.<name>]` table as TOML text, empty when the file has
        /// no such section
        toml: DogSectionToml,
    },
    /// Answer to `EnableDog`: the dog as it stands now
    DogStarted(ProcessInfo),
    /// Answer to `DogStaleness`: this daemon's own handshake record, split
    /// into the dogs it has given up on and the dogs it is still waiting on.
    ///
    /// Two lists because they answer two questions. `stale` is a finding;
    /// `pending` is a reason to ask again, since a reading taken now would
    /// be a guess about them.
    ///
    /// Names only: two builds differing only in the protocol they speak
    /// report the same crate version.
    DogStaleness {
        /// Dogs this daemon has refused twice: once on the handshake that
        /// bought them a restart from disk, and again after it. It will not
        /// restart them a third time.
        stale: Vec<String>,
        /// Dogs this daemon is still waiting to hear a final answer from: one
        /// whose restart is in flight, or one it supervises that has not
        /// handshook yet. Neither stale nor known healthy.
        pending: Vec<String>,
    },
    /// Answer to `HandoverFitness`: `None` when the whole flock can be
    /// carried across a daemon handover, and otherwise the sentence saying
    /// which sheep cannot be and why.
    ///
    /// A rendered sentence rather than a structured reason: the set of things
    /// a handover cannot carry keeps changing, and the client only prints it.
    HandoverFitness {
        /// Why the flock cannot be handed over in place, or `None` when it
        /// can.
        refusal: Option<String>,
    },
    /// Answer to `Subscribe`
    Subscribed,
    /// Answer to `KillDaemon`
    ShuttingDown,
}

#[cfg(test)]
mod tests {
    use super::super::process::sample_info;
    use super::super::{
        ActionOutcome, DogSource, ExitInfo, Lamb, LineOutcome, Reply, Request, RpcError,
        RpcErrorCode, SignalOutcome,
    };
    use super::*;
    use crate::config::AppConfig;
    use crate::status::ProcStatus;

    /// The reply carries key names and never a value.
    #[test]
    fn sheep_env_batch_response_wire_v8() {
        let response = Response::SheepEnvBatch {
            name: "web".to_string(),
            set: vec!["A".to_string()],
            unchanged: vec!["B".to_string()],
            collisions: Vec::new(),
        };
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"sheep_env_batch","data":{"name":"web","set":["A"],"unchanged":["B"],"collisions":[]}}"#
        );
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), response);
    }

    #[test]
    fn reply_wire_snapshots() {
        let replies = vec![
            Reply {
                id: 1,
                result: Ok(Response::Pong),
            },
            Reply {
                id: 2,
                result: Ok(Response::Flock(vec![sample_info()])),
            },
            Reply {
                id: 3,
                result: Err(RpcError {
                    code: RpcErrorCode::NotFound,
                    message: "no sheep matches `web`".to_string(),
                    daemon_version: None,
                }),
            },
            // `ActionReply` is not a `ProcessInfo`. `Replied` is the
            // struct-shaped `ActionOutcome` variant and so the one worth
            // pinning.
            Reply {
                id: 4,
                result: Ok(Response::Triggered(vec![ActionReply {
                    id: 3,
                    name: "web".to_string(),
                    outcome: ActionOutcome::Replied {
                        body: "ok".to_string(),
                    },
                }])),
            },
            // The only struct-shaped `Response` variant; every other one is
            // a newtype over a `Vec` or a unit, both proven above.
            Reply {
                id: 5,
                result: Ok(Response::RollSaved {
                    path: "/home/ada/.shep/flock.json".to_string(),
                    apps: 2,
                }),
            },
            // The present `dog` marker; `sample_info()` pins the absent one.
            // `Adopted` because it is the variant carrying a payload.
            Reply {
                id: 6,
                result: Ok(Response::Flock(vec![ProcessInfo {
                    id: 7,
                    name: "otel".to_string(),
                    dog: Some(DogSource::Adopted {
                        path: "/usr/local/bin/shep-otel".to_string(),
                    }),
                    ..sample_info()
                }])),
            },
            // The section crosses the wire as text, never a typed structure.
            Reply {
                id: 7,
                result: Ok(Response::DogSection {
                    toml: "port = 9615\n".to_string().into(),
                }),
            },
            // The only `Response` variant carrying a bare `ProcessInfo`
            // rather than a `Vec`: `enable` starts exactly one dog.
            Reply {
                id: 8,
                result: Ok(Response::DogStarted(ProcessInfo {
                    id: 4,
                    name: "metrics".to_string(),
                    dog: Some(DogSource::BuiltIn),
                    ..sample_info()
                })),
            },
            // Each row below carries the smallest body that shows its wire
            // shape: the tag is what is being pinned. `Deleted` is a
            // `Vec<u32>`; `Subscribed` and `ShuttingDown` carry nothing.
            Reply {
                id: 9,
                result: Ok(Response::Described(vec![])),
            },
            Reply {
                id: 10,
                result: Ok(Response::Started(vec![])),
            },
            Reply {
                id: 11,
                result: Ok(Response::Stopped(vec![])),
            },
            // Both halves populated, as the row below: `refused` is the one
            // field on either variant a walk fills and a single-target
            // request never does.
            Reply {
                id: 12,
                result: Ok(Response::Restarted {
                    accepted: vec![],
                    refused: vec![SheepRefusal::new(
                        "db",
                        "selector matched no registered sheep",
                    )],
                }),
            },
            Reply {
                id: 13,
                result: Ok(Response::Reloading {
                    accepted: vec![],
                    refused: vec![SheepRefusal::new("db", "db is already being reloaded")],
                }),
            },
            Reply {
                id: 14,
                result: Ok(Response::Deleted(vec![7, 8])),
            },
            Reply {
                id: 15,
                result: Ok(Response::Reopened(vec![])),
            },
            Reply {
                id: 16,
                result: Ok(Response::Flushed(vec![])),
            },
            Reply {
                id: 17,
                result: Ok(Response::Mustered(vec![])),
            },
            Reply {
                id: 18,
                result: Ok(Response::Subscribed),
            },
            Reply {
                id: 19,
                result: Ok(Response::ShuttingDown),
            },
            // `Signalled`, mirroring the `Triggered` row: one row per
            // `SignalOutcome` variant, so no tag is left unproven.
            Reply {
                id: 20,
                result: Ok(Response::Signalled(vec![
                    SignalReply {
                        id: 1,
                        name: "web".to_string(),
                        outcome: SignalOutcome::Delivered,
                    },
                    SignalReply {
                        id: 2,
                        name: "web".to_string(),
                        outcome: SignalOutcome::NotRunning,
                    },
                    SignalReply {
                        id: 3,
                        name: "api".to_string(),
                        outcome: SignalOutcome::Failed {
                            reason: "no such process".to_string(),
                        },
                    },
                ])),
            },
            Reply {
                id: 21,
                result: Ok(Response::Scaled(vec![sample_info()])),
            },
            // `SentLine`, mirroring the `Signalled` row: one row per
            // `LineOutcome` variant.
            Reply {
                id: 22,
                result: Ok(Response::SentLine(vec![
                    LineReply {
                        id: 1,
                        name: "repl".to_string(),
                        outcome: LineOutcome::Sent,
                    },
                    LineReply {
                        id: 2,
                        name: "web".to_string(),
                        outcome: LineOutcome::NoStdin,
                    },
                    LineReply {
                        id: 3,
                        name: "stuck".to_string(),
                        outcome: LineOutcome::NotWritten {
                            reason: "the app did not read its stdin within 2s".to_string(),
                        },
                    },
                ])),
            },
            // A walked lamb tree; every other row pins the `null` shape.
            Reply {
                id: 23,
                result: Ok(Response::Described(vec![
                    ProcessInfo::builder(3, "web", ProcStatus::Online)
                        .pid(Some(4242))
                        .lambs(Some(vec![Lamb::new(4243, "node"), Lamb::new(4244, "sh")]))
                        .build(),
                ])),
            },
            // The killed-by-signal shape of `last_exit`; every row above pins
            // the exited-normally one. `SIGTERM`'s raw number, since this
            // crate carries no name for it.
            Reply {
                id: 24,
                result: Ok(Response::Flock(vec![
                    ProcessInfo::builder(5, "worker", ProcStatus::Stopped)
                        .restarts(1)
                        .last_exit(Some(ExitInfo {
                            code: None,
                            signal: Some(15),
                        }))
                        .build(),
                ])),
            },
            // The one row that pins a smit on the wire; `sample_info()` carries
            // none.
            Reply {
                id: 25,
                result: Ok(Response::SmitPainted(vec![
                    ProcessInfo::builder(3, "web", ProcStatus::Online)
                        .pid(Some(4242))
                        .smit(Some("\u{25b2} main@a1b2c3".to_string()))
                        .build(),
                ])),
            },
            // A sheep drifting in one field and a sheep drifting in several.
            // `env` is one of them: the name travels and the value never does.
            Reply {
                id: 26,
                result: Ok(Response::Drifted(vec![
                    SheepDrift::new("web", vec!["cwd".to_string()]),
                    SheepDrift::new(
                        "api",
                        vec!["args".to_string(), "env".to_string(), "script".to_string()],
                    ),
                ])),
            },
            // The present shape of `instance`; every row above pins its absence.
            Reply {
                id: 27,
                result: Ok(Response::Flock(vec![
                    ProcessInfo::builder(9, "web", ProcStatus::Online)
                        .pid(Some(5150))
                        .instance(Some(2))
                        .build(),
                ])),
            },
            // Both shapes of the handover answer; the difference between them
            // is a `null`.
            Reply {
                id: 28,
                result: Ok(Response::HandoverFitness { refusal: None }),
            },
            Reply {
                id: 29,
                result: Ok(Response::HandoverFitness {
                    refusal: Some("sheep 'web' has a shepherd channel".to_string()),
                }),
            },
            // Both lists non-empty and different: the two carry the same wire
            // shape.
            Reply {
                id: 30,
                result: Ok(Response::DogStaleness {
                    stale: vec!["metrics".to_string()],
                    pending: vec!["bark".to_string()],
                }),
            },
            // A dog whose process is up and which has never answered this
            // shepherd. `dog_stale: false` is the silence still being waited
            // out; the row below is the one it has given up on.
            Reply {
                id: 31,
                result: Ok(Response::Flock(vec![
                    ProcessInfo::builder(10, "log-rotate", ProcStatus::Online)
                        .pid(Some(208_341))
                        .dog(Some(DogSource::Adopted {
                            path: "/usr/local/bin/shep-log-rotate".to_string(),
                        }))
                        .handshook(Some(false))
                        .dog_stale(Some(false))
                        .build(),
                ])),
            },
            Reply {
                id: 32,
                result: Ok(Response::Flock(vec![
                    ProcessInfo::builder(10, "log-rotate", ProcStatus::Online)
                        .pid(Some(208_341))
                        .dog(Some(DogSource::Adopted {
                            path: "/usr/local/bin/shep-log-rotate".to_string(),
                        }))
                        .handshook(Some(false))
                        .dog_stale(Some(true))
                        .build(),
                ])),
            },
            // Three entries, one per shape a load produces: applied, pending,
            // refused. `env` is a pending name on purpose: the name travels
            // and the value never does.
            Reply {
                id: 32,
                result: Ok(Response::Applied(vec![
                    SheepApplied::new("web", vec!["max_memory".to_string()], Vec::new(), None),
                    SheepApplied::new(
                        "api",
                        Vec::new(),
                        vec!["args".to_string(), "env".to_string()],
                        None,
                    ),
                    SheepApplied::new(
                        "worker",
                        Vec::new(),
                        Vec::new(),
                        Some("worker is not registered".to_string()),
                    ),
                ])),
            },
            // `Added`'s tag, all a fixture can prove for a `Vec<ProcessInfo>`
            // variant. Down here because every id in this vector is
            // hand-written.
            Reply {
                id: 33,
                result: Ok(Response::Added(vec![])),
            },
            // The config pane's answer, and the row that proves its whole
            // security property: `env` serializes as an empty object while
            // `env_keys` names the key beside it, so an out-of-tree reader
            // learns here that a value never travels (IR-41).
            Reply {
                id: 34,
                result: Ok(Response::SheepConfig(Box::new(SheepConfigView::new(
                    {
                        let mut config = AppConfig::minimal("web", "./srv");
                        config
                            .env
                            .insert("DATABASE_URL".to_string(), "postgres://x".to_string());
                        config
                    },
                    vec!["max_restarts".to_string()],
                    vec!["env".to_string()],
                )))),
            },
            // The three acknowledgements. None echoes what was written:
            // `SheepEnvSet` names the key and not its value, for the reason
            // the row above pins, `SheepFieldSet` does the same and adds
            // the one bit the caller cannot derive, and `DogConfigSet`
            // names the dog and not the section.
            Reply {
                id: 35,
                result: Ok(Response::SheepEnvSet {
                    name: "web".to_string(),
                    key: "DATABASE_URL".to_string(),
                }),
            },
            // `pending` pinned `true`, because `false` is the value a reader
            // that dropped the field entirely would decode by accident, and
            // the two answers send an operator to different places: one
            // says the change is in force, the other says to reload.
            Reply {
                id: 36,
                result: Ok(Response::SheepFieldSet {
                    name: "web".to_string(),
                    key: "script".to_string(),
                    pending: true,
                    // `None` on purpose: this is the row every earlier
                    // version's fixture already pinned, and `warning` must
                    // stay invisible on the wire for that value or the
                    // additive claim above is false.
                    warning: None,
                }),
            },
            Reply {
                id: 37,
                result: Ok(Response::DogConfigSet {
                    name: "bark".to_string(),
                }),
            },
            // A count and not the entries: a dog already knows what it
            // pushed, and echoing the map back would put every value it
            // sent on the wire a second time for no reader (IR-41).
            Reply {
                id: 38,
                result: Ok(Response::SecretsPut { accepted: 2 }),
            },
            // Key names only, on all three lists, and none empty here on
            // purpose: an empty `Vec` serializes the same whether it holds
            // strings or something else, so a reader that guessed the
            // element type wrong would still pass against an empty-list
            // fixture. `collisions` also proves a forced write can name a
            // key in both `set` and `collisions` at once.
            Reply {
                id: 39,
                result: Ok(Response::SheepEnvBatch {
                    name: "web".to_string(),
                    set: vec!["DATABASE_URL".to_string(), "API_KEY".to_string()],
                    unchanged: vec!["LOG_LEVEL".to_string()],
                    collisions: vec!["API_KEY".to_string()],
                }),
            },
            // Both halves of the same reply: a machine that has been read
            // and one that has not been read for long enough yet. The rates
            // are what separate them, and they are `null` rather than `0`
            // in the second, which is the whole distinction the renderer
            // spells as `-`.
            Reply {
                id: 40,
                result: Ok(Response::HostUsage(Some(HostUsage {
                    cpu_percent: Some(11.5),
                    memory_used_bytes: 39_963_869_184,
                    memory_total_bytes: 51_539_607_552,
                    disk_bytes_per_second: Some((1_258_291, 491_520)),
                    network_bytes_per_second: Some((24_594, 9_260)),
                }))),
            },
            Reply {
                id: 41,
                result: Ok(Response::HostUsage(Some(HostUsage {
                    cpu_percent: None,
                    memory_used_bytes: 39_963_869_184,
                    memory_total_bytes: 51_539_607_552,
                    disk_bytes_per_second: None,
                    network_bytes_per_second: None,
                }))),
            },
            // A platform `sysinfo` cannot read at all, which is neither of
            // the two above.
            Reply {
                id: 42,
                result: Ok(Response::HostUsage(None)),
            },
        ];
        insta::assert_json_snapshot!("reply_wire_v9", replies);
    }

    /// The additive claim on `SheepFieldSet::warning` has three halves and
    /// the reply fixture pins only the first. `None` staying off the wire is
    /// what makes the field additive at all; `Some` surviving a round trip is
    /// what makes it useful; and a payload with no `warning` key reading back
    /// as `None` is what lets a daemon built before the field answer a client
    /// built after it.
    ///
    /// The third pins the behaviour, not the attribute. Measured by removing
    /// `#[serde(default)]` and re-running: it still passes, because serde
    /// already reads a missing `Option` field as `None` without being asked.
    /// So the attribute is belt-and-braces and this test would not notice its
    /// removal. What it does notice is the field being renamed, retyped, or
    /// made required, which is what would actually break an older peer.
    #[test]
    fn the_set_field_warning_is_additive_in_both_directions() {
        let quiet = Response::SheepFieldSet {
            name: "web".to_string(),
            key: "script".to_string(),
            pending: true,
            warning: None,
        };
        let json = serde_json::to_string(&quiet).unwrap();
        assert!(
            !json.contains("warning"),
            "a `None` warning must not reach the wire at all: {json}"
        );

        let loud = Response::SheepFieldSet {
            name: "web".to_string(),
            key: "cwd".to_string(),
            pending: true,
            warning: Some("/srv/app does not exist yet".to_string()),
        };
        let json = serde_json::to_string(&loud).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, loud, "a warning must survive the round trip: {json}");

        let older =
            r#"{"kind":"sheep_field_set","data":{"name":"web","key":"script","pending":true}}"#;
        let back: Response = serde_json::from_str(older).unwrap();
        assert_eq!(
            back,
            Response::SheepFieldSet {
                name: "web".to_string(),
                key: "script".to_string(),
                pending: true,
                warning: None,
            },
            "a peer that predates the field must still deserialize"
        );
    }

    #[test]
    fn save_roll_serializes_snake_case_with_its_payload_under_data() {
        assert_eq!(
            serde_json::to_string(&Request::SaveRoll).unwrap(),
            r#"{"kind":"save_roll"}"#
        );
        let reply = Response::RollSaved {
            path: "/tmp/flock.json".to_string(),
            apps: 3,
        };
        let wire = r#"{"kind":"roll_saved","data":{"path":"/tmp/flock.json","apps":3}}"#;
        assert_eq!(serde_json::to_string(&reply).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Response>(wire).unwrap(), reply);
    }

    /// The listing is empty on purpose: `reply_wire_snapshots` pins the row
    /// field by field.
    #[test]
    fn muster_serializes_snake_case_with_its_listing_under_data() {
        assert_eq!(
            serde_json::to_string(&Request::Muster).unwrap(),
            r#"{"kind":"muster"}"#
        );
        let reply = Response::Mustered(Vec::new());
        let wire = r#"{"kind":"mustered","data":[]}"#;
        assert_eq!(serde_json::to_string(&reply).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Response>(wire).unwrap(), reply);
    }
}
