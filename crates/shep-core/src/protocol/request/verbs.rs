//! [`Request`], one variant per verb, and the selector a verb names a sheep with.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, DeclaredApp, DogTable, ResetDepth};

use super::{DogSectionToml, DogSource, EnvCopyChange, EnvValue, LogStream, SelectorSpec, Smit};

// Named by intra-doc links and by nothing rustc compiles, so the
// import is behind `cfg(doc)` rather than flagged unused.
#[cfg(doc)]
use super::{Response, RpcErrorCode, SheepApplied, SheepDrift};

/// One RPC request
// wire format: changing existing variants is a breaking change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    /// Liveness check
    Ping,
    /// Full flock listing
    ListFlock,
    /// What the machine the flock runs on is doing
    ///
    /// Answers [`Response::HostUsage`]. Its own request rather than a field
    /// on [`Response::Flock`]: that variant serializes as an array. A second
    /// field would retype it into an object no shipped client can read. An
    /// older daemon decodes [`Self::Unrecognized`] and refuses by name, costing the host block alone.
    HostUsage,
    /// Detailed info for matching sheep
    Describe {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Register + start apps
    Start {
        /// App configs. The daemon must re-normalize them, since peer input
        /// is untrusted; failures return [`RpcErrorCode::InvalidConfig`]
        apps: Vec<AppConfig>,
    },
    /// Register apps as flock members without starting any of them
    ///
    /// Each app lands `Stopped` and holds no pid; `shep add` is the verb.
    /// Idempotent by name: an app the flock already has is answered as it
    /// stands. Nothing about it changes. [`Self::ApplyConfig`] merges a
    /// template into one the flock already has, and `shep add` sends both.
    /// Answers [`Response::Added`].
    Add {
        /// App configs, carried exactly as [`Self::Start`] carries them. The
        /// daemon must re-normalize them, since peer input is untrusted;
        /// failures return [`RpcErrorCode::InvalidConfig`]
        apps: Vec<AppConfig>,
    },
    /// Ask which of `apps` name a sheep the flock already has under a different config
    ///
    /// Read-only. [`Self::Start`] on an already-registered name adds
    /// instances rather than reconciling config.
    ///
    /// Answers [`Response::Drifted`] with one [`SheepDrift`] per app that is
    /// both registered and different. An app the flock lacks is absent.
    ConfigDrift {
        /// The configs to compare against, exactly as [`Self::Start`] would
        /// carry them. The daemon must re-normalize them: peer input is
        /// untrusted, and an unnormalized config would report every default
        /// it has not spelled out as a difference. Failures return
        /// [`RpcErrorCode::InvalidConfig`].
        apps: Vec<AppConfig>,
    },
    /// Merge each declared app into the sheep of the same name, applying
    /// what can be applied and parking the rest for that sheep's next spawn
    ///
    /// Nothing is registered, pruned or killed. An app the flock does not
    /// have is refused by name. A field the running child was spawned from
    /// waits for a `shep reload`. Additive by default; `reset` widens it. Answers [`Response::Applied`].
    ApplyConfig {
        /// The apps to merge in, each carrying the keys its document
        /// literally wrote. The daemon must re-normalize the merge result,
        /// since peer input is untrusted, and refuses the whole request with
        /// [`RpcErrorCode::InvalidConfig`] when two entries share a name:
        /// the second would be merged against a store the first has not
        /// written yet.
        apps: Vec<DeclaredApp>,
        /// How much of what the operator has set since a template last
        /// loaded this request may overwrite. Default [`ResetDepth::None`],
        /// which overwrites nothing.
        ///
        /// Spelled `none`/`file`/`env`/`policy` on the wire.
        reset: ResetDepth,
    },
    /// One sheep's effective config, for a pane that is about to edit it.
    ///
    /// `env` comes back emptied and its key names ride separately, so a
    /// value never crosses the wire. Read-only: nothing about the sheep
    /// changes.
    ///
    /// Answers [`Response::SheepConfig`], or [`RpcErrorCode::NotFound`] when no sheep has that name.
    SheepConfig {
        /// The sheep's name, not a selector: a pane edits one sheep, for
        /// the reason [`Self::Scale`] states at length.
        name: String,
    },
    /// Sets, replaces, or with `None` removes one env key on one sheep,
    /// recorded as an operator override. Never reads it back.
    ///
    /// Its own request rather than a [`Self::ApplyConfig`] depth. No depth
    /// writes one key, and a pane is never told the values it would have to
    /// send back. The change parks for the next spawn, as a respawn-only
    /// field does. Answers [`Response::SheepEnvSet`].
    SetSheepEnv {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// The env key.
        key: String,
        /// The value, or `None` to remove the key.
        ///
        /// [`EnvValue`], not a bare `String`, for the reason that type's
        /// own doc gives: this is the most secret-dense field on the wire
        /// and a derived `Debug` on [`Request`] would print it (IR-41).
        value: Option<EnvValue>,
    },
    /// Sets several env keys on one sheep in a single write.
    ///
    /// `shep import env` is the caller. It writes twenty keys at once and
    /// must be able to refuse the whole set. The daemon applies it as one
    /// read-modify-write, so every key lands or none does. A different
    /// value already in place is a collision: `force` overwrites and names
    /// them. `dry_run` writes nothing. Parks for the next spawn.
    SetSheepEnvBatch {
        /// The sheep's name.
        name: String,
        /// The keys and their values.
        ///
        /// [`EnvValue`], not `String`, for [`Self::SetSheepEnv`]'s reason:
        /// `Request` derives `Debug` and this map is the densest run of
        /// secrets on the wire (IR-41).
        entries: BTreeMap<String, EnvValue>,
        /// Overwrite colliding keys instead of refusing.
        force: bool,
        /// Report what would happen and write nothing.
        dry_run: bool,
    },
    /// Copies one sheep's env onto another's, inside the shepherd.
    ///
    /// Its own request because a client is never sent an env value. It has
    /// no map to hand to [`Self::SetSheepEnvBatch`]. A `{{secret:...}}`
    /// value travels as the reference and never resolved.
    ///
    /// Merges by default; `replace` drops the keys only `to` had. `expect` is a dry run's list, and a mismatch refuses.
    CopySheepEnv {
        /// The sheep whose env is read.
        from: String,
        /// The sheep whose env is written.
        to: String,
        /// Make `to`'s env exactly `from`'s, removing what only `to` had.
        replace: bool,
        /// Keys left as `to` had them.
        except: Vec<String>,
        /// Report what would happen and write nothing.
        dry_run: bool,
        /// The change list the caller reviewed; the write is refused when
        /// the shepherd's own differs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<Vec<EnvCopyChange>>,
    },
    /// Sets one config field on one sheep, recorded as an operator
    /// override.
    ///
    /// [`Self::ApplyConfig`] can move one field, but moves it as a template
    /// and spends the operator's override. Through that route the `*` marker
    /// never appears for a pane's own edit. `env`, `name` and `instances`
    /// are refused here. The apply classification governs as for a load.
    SetSheepField {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// The [`AppConfig`] field to set. A key that type has no such
        /// field is refused with [`RpcErrorCode::InvalidConfig`] rather
        /// than ignored.
        key: String,
        /// The new value, in the shape that field serializes as. The daemon
        /// re-validates the resulting config: peer input is untrusted. It
        /// refuses with [`RpcErrorCode::InvalidConfig`] when the value does not
        /// deserialize or does not normalize. Nothing is written in either case.
        ///
        /// A bare [`serde_json::Value`], not a redacting newtype. This enum
        /// already prints `cwd`, `script` and `args` in the clear elsewhere.
        value: serde_json::Value,
    },
    /// Drops the operator's override of one config field, so the value the
    /// Flockfile gave it comes back.
    ///
    /// [`Self::SetSheepField`]'s undo. The value that returns is the one the
    /// field held before the operator spoke for it, or the built-in default.
    ///
    /// Refuses the keys [`Self::SetSheepField`] refuses, and a key nothing overrode: a silent no-op reads as success.
    ResetSheepField {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// The [`AppConfig`] field whose override to drop.
        key: String,
    },
    /// One dog's `[app.dogs.<name>]` table, for every sheep carrying one.
    ///
    /// It reads the stored spec, in force because `dogs` is
    /// [`ApplyGroup::Live`](crate::config::ApplyGroup::Live). A sheep with
    /// no table for `dog` is absent, and `dog` is self-declared.
    ///
    /// Answers [`Response::DogSheepSettings`] with an empty map, never [`RpcErrorCode::NotFound`].
    DogSheepSettings {
        /// The dog's own name, the config key.
        dog: String,
    },
    /// Sets, replaces, or with `None` removes one dog's table on one sheep,
    /// recorded as an operator override of the whole `dogs` field so the
    /// `*` marker shows, and publishes `config.sheep.<dog>` so a running
    /// dog re-reads its tables.
    ///
    /// A single dog's table, not the whole `dogs` map: a whole-map write
    /// would overwrite a concurrent edit to another dog's table.
    SetSheepDogSettings {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// Which dog's table to write.
        dog: String,
        /// The new table, or `None` to remove it.
        table: Option<DogTable>,
    },
    /// One dog's operator-set default table, the one every sheep carrying
    /// a table for `dog` layers its own over.
    ///
    /// Answers [`Response::DogDefaults`]; `None` when the dog has no default.
    /// Additive: an older shepherd answers `Unsupported`.
    DogDefaults {
        /// The dog's own name, the config key.
        dog: String,
    },
    /// Sets, replaces, or with `None` removes one dog's default table, and
    /// publishes `config.sheep.<dog>` for each sheep whose layered table
    /// moved.
    ///
    /// Held in `$SHEP_HOME/dog-defaults.json`, not in any sheep's `dogs`
    /// field. Answers [`Response::DogDefaultsSet`].
    SetDogDefaults {
        /// Which dog's default to write.
        dog: String,
        /// The new default, or `None` to remove it.
        table: Option<DogTable>,
    },
    /// Replaces one dog's `[<name>]` section in `dogs.toml` and publishes
    /// `config.dog.<name>` so a running dog re-reads it.
    ///
    /// The writing twin of [`Self::DogConfig`], which reads the same
    /// section.
    ///
    /// Answers [`Response::DogConfigSet`].
    SetDogConfig {
        /// The dog's name, the config key.
        name: String,
        /// The whole section, as TOML text.
        ///
        /// [`DogSectionToml`], not a bare `String`, for the reason that
        /// type's own doc gives: a section can hold a dog's credentials and
        /// this is what keeps them out of a `{:?}` (IR-41).
        toml: DogSectionToml,
    },
    /// A provider dog's values for one namespace and one environment.
    ///
    /// Replaces that pair rather than merging into it, so a key deleted at
    /// the provider disappears here on the next push instead of lingering.
    ///
    /// `namespace` is bookkeeping, not authorization: `Hello::dog_name` is
    /// self-declared and nothing checks it against the spawn. One offender refuses the whole push.
    PutSecrets {
        /// The dog's registered name.
        namespace: String,
        /// Which environment these values are for.
        environment: String,
        /// The values, keyed by secret name. [`EnvValue`] so a `{:?}` of
        /// this request cannot print them.
        entries: BTreeMap<String, EnvValue>,
    },
    /// Stop matching sheep (stay registered)
    Stop {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Restart matching sheep
    Restart {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Replace each matching sheep with a fresh instance of the same app.
    /// Instances of one app are replaced one at a time, so the app can stay
    /// reachable across the swap
    Reload {
        /// Which sheep. No default: a reload replaces running processes.
        selector: SelectorSpec,
    },
    /// Stop matching sheep and refuse to spawn them again until `Enable`.
    ///
    /// The mark belongs to the app, so a selector naming an instance (an id
    /// or a slot) is widened to that instance's app. Answers
    /// [`Response::Disabled`].
    Disable {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Clear the disabled mark on matching sheep and start them.
    ///
    /// A selector matching a sheep that is not disabled changes nothing.
    /// Answers [`Response::Enabled`].
    Enable {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Stop + deregister matching sheep
    Delete {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Set how many instances one app runs (see `shep stock`).
    ///
    /// Takes a name where every other verb takes a [`SelectorSpec`].
    /// `instances` is a per-app number. A selector matching two apps could
    /// mean four of each or four in total. The count is absolute, not a delta.
    Scale {
        /// The app's name, exactly as its config spells it. Not a selector: no
        /// `all`, no regex, no `fold:`.
        name: String,
        /// How many instances the app has when this returns. `0` is refused
        /// with [`RpcErrorCode::InvalidConfig`]: `shep delete` is the verb
        /// for removing an app.
        count: u32,
    },
    /// Attach a short marker to `sheep` for `shep flock` to paint, or clear
    /// it with `None`.
    ///
    /// By name, not a selector: a smit belongs to a sheep. Every instance of
    /// that name shows it, one spawned after the paint included. Held in
    /// memory, scoped to the connection that sent it.
    SetSmit {
        /// Which sheep.
        sheep: String,
        /// The marker, or `None` to clear it.
        smit: Option<Smit>,
    },
    /// Reopen every matched sheep's log files, for an external rotator that
    /// has renamed them (`create`-mode rotation)
    Reopen {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Empty every matched sheep's log files: flush what is still pending,
    /// then truncate the recorded paths
    Flush {
        /// Which sheep. No default: this destroys log data.
        selector: SelectorSpec,
    },
    /// Zero the restart count of every matched sheep and hand back its
    /// restart budget.
    ///
    /// Changes counters only: nothing is signalled, started or stopped.
    ///
    /// Answers [`Response::Reset`]. Additive: an unknown variant decodes
    /// [`Self::Unrecognized`] and refuses by name.
    Reset {
        /// Which sheep. No default.
        selector: SelectorSpec,
    },
    /// Read the end of one sheep's out or err log, so a client on another
    /// host reads what the shepherd's own disk holds.
    ///
    /// Names the file by sheep and stream and never by path. This is not a
    /// way to make the shepherd read an arbitrary file. It sees only what
    /// the log pump has flushed. Answers [`Response::LogTail`]. A missing file is an answer, not an error.
    TailLog {
        /// The sheep's id; a dog's id counts
        id: u32,
        /// Which of its two log files
        stream: LogStream,
        /// How many lines to return at most
        lines: u32,
    },
    /// Send a named action to every matched sheep over its shepherd channel
    /// and report what each app says back (see `shep trigger`).
    Trigger {
        /// Which sheep. No default, matching every other verb that reaches
        /// a running process.
        selector: SelectorSpec,
        /// The action name. Free-form: the daemon never declares, parses, or
        /// validates it, and an app that does not recognize the name is
        /// expected to say so in its own reply.
        action: String,
        /// Argument text, passed through to the app verbatim. One opaque
        /// string, matching the shepherd channel's own `action` message this
        /// becomes.
        params: Option<String>,
    },
    /// Deliver one signal to every matched sheep's own process, never its
    /// process group, or to one of its lambs (see `shep signal`).
    Signal {
        /// Which sheep. No default, matching every other verb that reaches
        /// a running process.
        selector: SelectorSpec,
        /// The signal's name, as
        /// [`OperatorSignal`](crate::signals::OperatorSignal) spells it. The
        /// `SIG` prefix and the case are both optional; a name outside the
        /// grammar answers [`RpcErrorCode::InvalidConfig`].
        signal: String,
        /// Signal this lamb of the matched sheep instead of the sheep itself.
        ///
        /// The selector must then match exactly one sheep, and the pid must be
        /// one the shepherd finds in that sheep's lamb walk when the request
        /// arrives. Absent on the wire when `None`. A shepherd below protocol
        /// 13 drops the key and signals the sheep, so a client checks the
        /// peer's protocol first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lamb: Option<u32>,
    },
    /// Write one line to every matched sheep's stdin (see `shep whisper`).
    SendLine {
        /// Which sheep. No default, matching every other verb that reaches a
        /// running process.
        selector: SelectorSpec,
        /// The line, without its terminator: the shepherd appends exactly
        /// one `\n` when it writes.
        ///
        /// A line containing an embedded newline is refused
        /// ([`RpcErrorCode::InvalidConfig`]): it would deliver two commands
        /// where the operator typed one.
        line: String,
    },
    /// Write the muster roll now, bypassing the snapshot writer's debounce
    SaveRoll,
    /// Assemble the flock from the muster roll on disk: start every app the
    /// roll recorded running, leaving every app the flock already has exactly
    /// as it stands
    Muster,
    /// Ask for one dog's `[dog.<name>]` section, as the dog itself parses it
    ///
    /// The text as the operator wrote it: a `{{secret:...}}` reference comes
    /// back as a reference. An editor must use this one, since Lookout
    /// writes the whole text back through [`Self::SetDogConfig`].
    ///
    /// Answers [`Response::DogSection`].
    DogConfig {
        /// The dog's name: the config key, not a selector
        name: String,
    },
    /// Ask for one dog's section with every `{{secret:...}}` reference
    /// replaced by its value, resolved in the shepherd's own environment
    ///
    /// For a dog that consumes its configuration. [`Self::DogConfig`] stays
    /// the raw read. A reference that cannot resolve fails with
    /// [`RpcErrorCode::InvalidConfig`]. It names the key path and the
    /// reference, never a value. `dogs.toml` is never rewritten. Answers [`Response::DogSection`].
    DogConfigResolved {
        /// The dog's name: the config key, not a selector
        name: String,
    },
    /// Ask for one sheep's `[app.dogs.<dog>]` table with every
    /// `{{secret:...}}` reference replaced by its value, resolved in that
    /// sheep's own `environment` (the shepherd's when the sheep sets none)
    ///
    /// For a dog that consumes a per-sheep credential. One sheep at a time,
    /// so a bad reference refuses only its own table. Nothing is written. Answers [`Response::SheepDogTable`].
    SheepDogSettingsResolved {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// Which dog's table to read.
        dog: String,
    },
    /// Start one dog now, marking it as coming from `source` and spawning it
    /// with `env`
    EnableDog {
        /// The dog's name
        name: String,
        /// Where its binary comes from
        source: DogSource,
        /// The dog's operator-set environment, as written in `shep.toml`
        ///
        /// The CLI owns `shep.toml` and the shepherd never reads it, so the
        /// half of an `[daemon.adopted_dogs]` entry this daemon spawns with
        /// arrives here rather than on `source`, which every flock row
        /// carries. A value may be a `{{secret:...}}`, still a reference here.
        /// Empty for a built-in dog, and absent on the wire when it is.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, EnvValue>,
    },
    /// Stop and deregister one dog
    ///
    /// Answers [`Response::Deleted`]: disabling deregisters exactly as
    /// `Delete` does.
    DisableDog {
        /// The dog's name
        name: String,
    },
    /// Ask which dogs this daemon has given up on, and which it is still
    /// waiting to hear from (`shep daemon reload`).
    ///
    /// Read-only, and about this daemon's own handshakes: take the reading
    /// after a reload, not before one. Never sent to an older daemon, on
    /// [`Self::HandoverFitness`]'s terms. Answers [`Response::DogStaleness`].
    DogStaleness,
    /// Ask whether this daemon could hand its flock to a successor in place,
    /// rather than stopping it and starting it again (`shep daemon reload`).
    ///
    /// Read-only: the handover itself is triggered by a signal, which
    /// reaches a daemon that refuses the client at the handshake.
    ///
    /// A refusal is a feature the running daemon cannot carry, not an error. Answers [`Response::HandoverFitness`].
    HandoverFitness,
    /// Graceful daemon shutdown
    KillDaemon,
    /// Subscribe this connection to bus topics (glob patterns)
    Subscribe {
        /// Topic globs, e.g. `process.*`
        topics: Vec<String>,
    },
    /// A request kind this build has not been taught.
    ///
    /// `#[serde(other)]`, which serde allows here because `Request` is
    /// internally tagged and this variant carries nothing. The unknown
    /// body's own fields are discarded: the only thing to do with a
    /// request we cannot name is refuse it, and the refusal needs the
    /// envelope's id rather than the body.
    #[serde(other)]
    Unrecognized,
}

#[cfg(test)]
mod tests;
