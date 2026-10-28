//! [`Response`], one variant per request kind, and the host reading one of them carries.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::DogTable;

use super::{
    ActionReply, DogSectionToml, EnvCopyChange, LineReply, LogTail, ProcessInfo, SheepApplied,
    SheepConfigView, SheepDrift, SheepRefusal, SignalReply,
};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Request, RpcErrorCode, sort_flock};
#[cfg(doc)]
use crate::config::AppConfig;

/// The machine the flock runs on, as the shepherd last read it.
///
/// Three of the four numbers are rates. CPU, disk traffic and network
/// traffic are differences between two readings of a counter the kernel
/// only increments. The shepherd keeps the earlier reading between ticks.
///
/// Answered by [`Response::HostUsage`], and by nothing else.
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
/// Ten variants carry a bare `Vec<ProcessInfo>`. Do not collapse them
/// into one. Each names the request it answers, so a variant can diverge
/// without a protocol bump. `Reloading` means an acceptance rather than a
/// result; `Scaled` carries only the survivors of a scale-down.
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
    /// Boxed, and the only variant here that is. It carries a whole
    /// [`AppConfig`], and a `Response` rides inside every frame the daemon
    /// sends. `Box<T>` serializes exactly as `T`. The enum-level
    /// `#[allow(clippy::large_enum_variant)]` does not cover it.
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
    /// What a [`Request::CopySheepEnv`] did, or would have done.
    ///
    /// Key names, and values only as [`EnvCopyChange`] allows: a reference as
    /// written, anything else redacted. Never the resulting env map (IR-41).
    SheepEnvCopied {
        /// The sheep the env was read from.
        from: String,
        /// The sheep the env was written to.
        to: String,
        /// One entry per key the copy touches, in key order. Empty when `to`
        /// already holds everything the copy would write.
        changes: Vec<EnvCopyChange>,
    },
    /// Answer to `SetSheepField`: which field moved, and whether the
    /// running child has it.
    ///
    /// Not [`Self::Applied]'s three lists: this request names one field of
    /// one sheep. A second way to say no beside the `Err` arm would only be
    /// a trap. The remaining bit is not the field's own
    /// [`ApplyGroup`](crate::config::ApplyGroup).
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
        /// Advisory, not a second way to say no: the write landed.
        /// `normalize` cannot see the filesystem, so this is checked
        /// daemon-side, after the value is accepted. Additive: absent rather than `null` on an older peer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    /// Answer to `ResetSheepField`: which field went back, to what, and
    /// whether the running child has it.
    ///
    /// [`Self::SheepFieldSet`]'s twin, with the value the field now holds
    /// beside it, since the caller asked for the Flockfile's value without
    /// being told it.
    SheepFieldReset {
        /// The sheep.
        name: String,
        /// The field that went back.
        key: String,
        /// The value the field holds now, in the shape it serializes as.
        value: serde_json::Value,
        /// As [`Self::SheepFieldSet`]'s `pending`.
        pending: bool,
        /// As [`Self::SheepFieldSet`]'s `warning`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
    /// Answer to `DogSheepSettings`: one dog's table from every sheep
    /// carrying one.
    ///
    /// Empty rather than absent for a sheep naming no table. Empty rather
    /// than [`RpcErrorCode::NotFound`] for a dog nobody names: a dog asks
    /// this on its own name. [`DogTable`]'s manual `Debug` redacts the values wherever it rides (IR-41).
    DogSheepSettings {
        /// Sheep name to that sheep's table for this dog.
        tables: BTreeMap<String, DogTable>,
    },
    /// Answer to `SheepDogSettingsResolved`: one sheep's table for one dog,
    /// each `{{secret:...}}` reference replaced by its value.
    ///
    /// `None` when the sheep carries no table for the dog. Printing is the
    /// same as [`Self::DogSheepSettings`]: [`DogTable`]'s manual `Debug`
    /// keeps a resolved value out of a `{:?}` (IR-41).
    SheepDogTable {
        /// The table, or `None` when the sheep carries none for this dog.
        table: Option<DogTable>,
    },
    /// Answer to `SetSheepDogSettings`: which sheep and dog moved.
    ///
    /// Names only, like [`Self::SheepEnvSet`] and [`Self::DogConfigSet`]:
    /// the table just written is the caller's own, so echoing it back
    /// would put it on the wire a second time for no reader (IR-41).
    SheepDogSettingsSet {
        /// The sheep.
        name: String,
        /// The dog whose table moved.
        dog: String,
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
    /// Answer to `Disable`: every instance of each app now disabled, as
    /// they stand once stopped.
    Disabled(Vec<ProcessInfo>),
    /// Answer to `Enable`: every instance of each app that was disabled and
    /// is now started.
    ///
    /// Empty when the selector matched only apps that were not disabled.
    Enabled(Vec<ProcessInfo>),
    /// Answer to `Restart`: the sheep that were restarted, one row each.
    ///
    /// When the reply arrives depends on how many sheep matched. One sheep is
    /// answered as soon as its respawn is issued. Two or more are restarted
    /// in dependency order. The daemon holds each stage until the apps a
    /// later stage waits on are back. A client's budget covers the whole walk.
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
    /// One instance costs a readiness wait plus a drain. Every row is the
    /// sheep as it stood when its reload was accepted. The swaps report
    /// themselves on the bus (`process.reload`, `process.reloaded`,
    /// `process.reload_abandoned`). Two or more sheep reload in dependency
    /// order, stage by stage.
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
    /// Answer to `Reset`: one row per matched sheep after the reset, running
    /// or not, exactly as [`Self::Reopened`].
    Reset(Vec<ProcessInfo>),
    /// Answer to `TailLog`: the end of one sheep's log file, or the reason
    /// there is none to read.
    LogTail(LogTail),
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
    /// `pending` is a reason to ask again.
    ///
    /// Names only: two builds differing only in the protocol they speak report the same crate version.
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
mod tests;
