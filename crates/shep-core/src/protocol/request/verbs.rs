//! [`Request`], one variant per verb, and the selector a verb names a sheep with.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, DeclaredApp, DogTable, ResetDepth};

use super::{DogSectionToml, DogSource, EnvValue, SelectorSpec, Smit};

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
    /// on [`Response::Flock`]: that variant serializes as an array, and
    /// giving it a second field would retype it into an object that no
    /// shipped client can read. A daemon that has never heard of this one
    /// decodes [`Self::Unrecognized`] and refuses by name, which costs the
    /// caller the host block and nothing else.
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
    ///
    /// Idempotent by name: an app the flock already has is answered as it
    /// stands, running or not, and nothing about it changes.
    /// [`Self::ApplyConfig`] merges a template into one the flock already
    /// has, and `shep add` sends both.
    ///
    /// Answers [`Response::Added`].
    Add {
        /// App configs, carried exactly as [`Self::Start`] carries them. The
        /// daemon must re-normalize them, since peer input is untrusted;
        /// failures return [`RpcErrorCode::InvalidConfig`]
        apps: Vec<AppConfig>,
    },
    /// Ask which of `apps` name a sheep the flock already has under a
    /// different config
    ///
    /// Read-only. [`Self::Start`] on an already-registered name adds
    /// instances rather than reconciling config.
    ///
    /// Answers [`Response::Drifted`] with one [`SheepDrift`] per app that is
    /// both registered and different. An app the flock does not have is
    /// absent from the answer, not reported as unchanged.
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
    /// Nothing is registered, nothing is pruned and nothing running is
    /// killed: an app the flock does not have is refused by name, and a
    /// field the running child was spawned from waits for a `shep reload`.
    /// Additive by default; `reset` widens it.
    ///
    /// Answers [`Response::Applied`] with one [`SheepApplied`] per entry in
    /// `apps`, in the order given, found or not and changed or not. One
    /// app's refusal rides in [`SheepApplied::refused`] and does not cost
    /// the rest of the file its load.
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
    /// Answers [`Response::SheepConfig`], or
    /// [`RpcErrorCode::NotFound`] when no sheep has that name.
    SheepConfig {
        /// The sheep's name, not a selector: a pane edits one sheep, for
        /// the reason [`Self::Scale`] states at length.
        name: String,
    },
    /// Sets, replaces, or with `None` removes one env key on one sheep,
    /// recorded as an operator override. Never reads it back.
    ///
    /// Its own request rather than a [`Self::ApplyConfig`] depth, because
    /// no depth does this: `ResetDepth::None` appends only, `File` and
    /// `Policy` leave env alone, and `Env`/`All` replace the whole map with
    /// the template's. A pane cannot send the whole map, since it is never
    /// told the values it would have to send back.
    ///
    /// The running child holds the env it was spawned from, so the change
    /// parks for the next spawn exactly as `ApplyConfig` parks a
    /// respawn-only field, and `shep reload`/`shep restart` promote it.
    ///
    /// Answers [`Response::SheepEnvSet`], or
    /// [`RpcErrorCode::NotFound`] when no sheep has that name.
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
    /// [`Self::SetSheepEnv`]'s doc says a request taking a map would need
    /// per-key reporting back "for no caller that wants it". `shep import
    /// env` is that caller: it writes twenty keys at once and has to refuse
    /// the whole set rather than leave eleven of them applied.
    ///
    /// The daemon applies this as one read-modify-write of the override
    /// store, so every key lands or none does. No removal arm: a pane
    /// deletes rows one at a time through [`Self::SetSheepEnv`], and an
    /// import never removes anything.
    ///
    /// A key already holding a different value is a collision. Without
    /// `force` any collision refuses the whole request and writes nothing;
    /// with it, the collisions are overwritten and named in the reply as
    /// well as counted in `set`. A key already holding the same value is
    /// `unchanged`, never a collision, so re-running an unchanged import
    /// needs no flag.
    ///
    /// `dry_run` computes the three lists and writes nothing.
    ///
    /// Parks for the next spawn, exactly as [`Self::SetSheepEnv`] does.
    ///
    /// Answers [`Response::SheepEnvBatch`], or
    /// [`RpcErrorCode::NotFound`] when no sheep has that name.
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
    /// Sets one config field on one sheep, recorded as an operator
    /// override.
    ///
    /// [`Self::SetSheepEnv`]'s twin for everything that is not `env`, and
    /// it exists for the reason that one does rather than by symmetry.
    /// [`Self::ApplyConfig`] can move a single field (one [`DeclaredApp`]
    /// declaring one key, at [`ResetDepth::File`]), but it moves it as a
    /// template and spends the operator's override for it. That reasoning
    /// does not hold here: a pane's value is the operator's, and the sheep
    /// still differs from its file. Routed through `ApplyConfig`, the `*`
    /// marker would never appear for that edit.
    ///
    /// One field, not a map: a pane edits one row at a time, and a request
    /// that took several would need [`Response::Applied`]'s per-field
    /// reporting back again for no caller that wants it.
    ///
    /// `env` is refused here and goes through [`Self::SetSheepEnv`]. So are
    /// `name` and `instances`, which are
    /// [`ApplyGroup::Structural`](crate::config::ApplyGroup::Structural):
    /// identity and flock shape rather than runtime knobs, and the count
    /// moves through [`Self::Scale`].
    ///
    /// The four-way apply classification governs exactly as it does for a
    /// load. A `Live` field is in force at the daemon's next decision, a
    /// `NextSpawn` field reaches the stored spec, and a `NeedsRespawn`
    /// field parks for `shep reload` to promote.
    ///
    /// Answers [`Response::SheepFieldSet`], or
    /// [`RpcErrorCode::NotFound`] when no sheep has that name.
    SetSheepField {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// The [`AppConfig`] field to set. A key that type has no such
        /// field is refused with [`RpcErrorCode::InvalidConfig`] rather
        /// than ignored.
        key: String,
        /// The new value, in the shape that field serializes as. The daemon
        /// must re-validate the resulting config (peer input is untrusted)
        /// and refuses with [`RpcErrorCode::InvalidConfig`] when it does not
        /// deserialize or does not normalize; nothing is written in either
        /// case.
        ///
        /// A bare [`serde_json::Value`] and not a redacting newtype, unlike
        /// [`Self::SetSheepEnv`]'s [`EnvValue`], and the asymmetry is
        /// deliberate. `env` is the one field [`AppConfig`]'s own manual
        /// `Debug` redacts; `cwd`, `script` and `args` are printed in the
        /// clear by every request that already carries a whole config
        /// ([`Self::Start`], [`Self::Add`], [`Self::ApplyConfig`]). A
        /// newtype here would protect one copy of a value this enum prints
        /// three other ways, which reads as a guarantee the wire does not
        /// make. Widening that protection is a change to [`AppConfig`]'s
        /// `Debug`, not to this field.
        value: serde_json::Value,
    },
    /// One dog's `[app.dogs.<name>]` table, for every sheep carrying one.
    ///
    /// It reads the stored spec, which is in force because `dogs` is
    /// [`ApplyGroup::Live`](crate::config::ApplyGroup::Live). A sheep with
    /// no table for `dog` is absent. `dog` is self-declared, as it is for
    /// [`Self::DogConfig`], so the scoping is a convenience: the boundary
    /// is the socket.
    ///
    /// Answers [`Response::DogSheepSettings`] with an empty map when no
    /// sheep names `dog`, never [`RpcErrorCode::NotFound`]: unlike
    /// [`Self::SheepConfig`], this does not name one sheep that could be
    /// missing.
    DogSheepSettings {
        /// The dog's own name, the config key.
        dog: String,
    },
    /// Sets, replaces, or with `None` removes one dog's table on one sheep,
    /// recorded as an operator override of the whole `dogs` field so the
    /// `*` marker shows, and publishes `config.sheep.<dog>` so a running
    /// dog re-reads its tables.
    ///
    /// A single dog's table, not the whole `dogs` map:
    /// [`Self::SetSheepField`] refuses the key `dogs` and names this
    /// request, the way it refuses `env`, since a whole-map write from a
    /// pane editing one dog would overwrite a concurrent edit to another
    /// dog's table on the same sheep.
    ///
    /// Answers [`Response::SheepDogSettingsSet`], [`RpcErrorCode::NotFound`]
    /// when no sheep has `name`, and refuses a dog's own name the way
    /// [`Self::SetSheepField`] does.
    SetSheepDogSettings {
        /// The sheep's name, not a selector, for [`Self::SheepConfig`]'s
        /// reason.
        name: String,
        /// Which dog's table to write.
        dog: String,
        /// The new table, or `None` to remove it.
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
    /// `namespace` is the dog's own registered name. It is bookkeeping, not
    /// authorization: `Hello::dog_name` is self-declared and nothing checks
    /// it against the spawn. The boundary is the socket itself, which lives
    /// under `$SHEP_HOME` at `0700`.
    ///
    /// The two names and every entry key are checked against
    /// [`crate::secrets::is_name`], and a value against
    /// [`crate::secrets::MAX_VALUE_BYTES`], the same cap the operator's own
    /// store enforces. One offender refuses the whole push rather than
    /// dropping its own entry, so a dog never reads `accepted` for a set
    /// that was stored in part.
    ///
    /// Answers [`Response::SecretsPut`].
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
    /// Replace each matching sheep with a fresh instance of the same app, one
    /// instance of an app at a time, so the app has a window in which it can
    /// stay reachable across the swap
    Reload {
        /// Which sheep. No default: a reload replaces running processes.
        selector: SelectorSpec,
    },
    /// Stop + deregister matching sheep
    Delete {
        /// Which sheep
        selector: SelectorSpec,
    },
    /// Set how many instances one app runs (see `shep stock`).
    ///
    /// Takes a name where every other verb takes a [`SelectorSpec`]:
    /// `instances` is a per-app number and slots are allocated against the
    /// same-name group, so a selector matching two apps could mean four of
    /// each or four in total.
    ///
    /// The count is absolute: two operators sending `+2` against the same
    /// app would get a number neither asked for.
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
    /// By name, not a selector: a smit belongs to a sheep, and every
    /// instance of that name shows it, one spawned after the paint included.
    ///
    /// Held in memory and scoped to the connection that sent it, so a
    /// publisher republishes rather than publishing on change.
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
    /// process group (see `shep signal`).
    Signal {
        /// Which sheep. No default, matching every other verb that reaches
        /// a running process.
        selector: SelectorSpec,
        /// The signal's name, as
        /// [`OperatorSignal`](crate::signals::OperatorSignal) spells it. The
        /// `SIG` prefix and the case are both optional; a name outside the
        /// grammar answers [`RpcErrorCode::InvalidConfig`].
        signal: String,
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
    /// Answer a question a sheep has open (see `shep answer`).
    ///
    /// Refused with [`RpcErrorCode::NotFound`] when the selector matches no
    /// sheep, or when the matched sheep has no open question with that id.
    /// Refused with [`RpcErrorCode::InvalidConfig`], naming the rule, when
    /// `question`, `answer`, `note`, `via` or `who` breaks the grammar in
    /// `shep_channel`, or when the answer is not one the question takes.
    Answer {
        /// Which sheep. It must match exactly one: an answer belongs to a
        /// single question.
        selector: SelectorSpec,
        /// The id of the open question. A `String`, not a `QuestionId`, so
        /// a malformed id is refused here with the rule it broke instead of
        /// failing the whole frame's decode.
        question: String,
        /// The answer: `yes` or `no` for a yes-or-no question, any text
        /// otherwise.
        answer: String,
        /// A remark delivered with a yes-or-no answer. A note on a text
        /// question is refused.
        note: Option<String>,
        /// The channel the answer came through, such as a chat bridge's
        /// name. Recorded with the settlement.
        via: Option<String>,
        /// Who answered. Recorded with the settlement.
        who: Option<String>,
    },
    /// Write the muster roll now, bypassing the snapshot writer's debounce
    SaveRoll,
    /// Assemble the flock from the muster roll on disk: start every app the
    /// roll recorded running, leaving every app the flock already has exactly
    /// as it stands
    Muster,
    /// Ask for one dog's `[dog.<name>]` section, as the dog itself parses it
    DogConfig {
        /// The dog's name: the config key, not a selector
        name: String,
    },
    /// Start one dog now, marking it as coming from `source`
    EnableDog {
        /// The dog's name
        name: String,
        /// Where its binary comes from
        source: DogSource,
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
    /// [`Self::HandoverFitness`]'s terms.
    ///
    /// Answers [`Response::DogStaleness`].
    DogStaleness,
    /// Ask whether this daemon could hand its flock to a successor in place,
    /// rather than stopping it and starting it again (`shep daemon reload`).
    ///
    /// Read-only: the handover itself is triggered by a signal, which reaches
    /// a daemon that refuses the client at the handshake.
    ///
    /// Answers [`Response::HandoverFitness`]. A refusal is a feature the
    /// running daemon cannot carry, not an error: the caller falls back to a
    /// stop-and-start and prints the reason. Never sent to an older daemon:
    /// shep-cli's `commands::daemon` gates it on the crate version the
    /// handshake reported.
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
mod tests {
    use super::super::{LineOutcome, LineReply, Response, SignalOutcome, SignalReply};
    use super::*;
    use crate::protocol::MIN_SUPPORTED;

    #[test]
    fn a_signal_request_and_its_reply_round_trip() {
        let request = Request::Signal {
            selector: SelectorSpec::Name("web".to_string()),
            signal: "SIGHUP".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);

        let reply = Response::Signalled(vec![
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
        ]);
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), reply);
        // The three tags, spelled out: a variant renamed in Rust changes
        // these strings, compiles clean, and breaks a client matching on them.
        assert!(json.contains(r#""kind":"delivered""#), "{json}");
        assert!(json.contains(r#""kind":"not_running""#), "{json}");
        assert!(json.contains(r#""kind":"failed""#), "{json}");
    }

    /// `instances` is a per-app number, so `shep stock /web.*/ 4` could mean
    /// four each or four total.
    #[test]
    fn a_scale_request_names_one_app_and_a_count() {
        let request = Request::Scale {
            name: "web".to_string(),
            count: 4,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        assert!(json.contains(r#""kind":"scale""#), "{json}");
        assert!(json.contains(r#""name":"web""#), "{json}");
        // No `selector` key at all: this verb is not one of the
        // selector-taking family.
        assert!(!json.contains("selector"), "{json}");
    }

    #[test]
    fn a_scaled_reply_carries_its_own_tag() {
        let json = serde_json::to_string(&Response::Scaled(vec![])).unwrap();
        assert_eq!(json, r#"{"kind":"scaled","data":[]}"#);
    }

    /// `Add` and `Start` carry byte-identical payloads and differ by their
    /// `kind` alone.
    #[test]
    fn an_add_request_and_its_reply_round_trip() {
        let request = Request::Add {
            apps: vec![AppConfig::minimal("web", "./srv")],
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        assert!(json.contains(r#""kind":"add""#), "{json}");

        let reply = Response::Added(vec![]);
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), reply);
        assert!(json.contains(r#""kind":"added""#), "{json}");
    }

    /// `NotWritten`'s reason is the only thing separating "the app is not
    /// reading its stdin" from "the pipe broke".
    #[test]
    fn a_send_line_request_and_its_reply_round_trip() {
        let request = Request::SendLine {
            selector: SelectorSpec::Name("repl".to_string()),
            line: "reload-config".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);

        let reply = Response::SentLine(vec![
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
        ]);
        let json = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), reply);
        assert!(json.contains(r#""kind":"sent""#), "{json}");
        assert!(json.contains(r#""kind":"no_stdin""#), "{json}");
        assert!(json.contains("did not read its stdin"), "{json}");
    }

    #[test]
    fn a_line_carrying_a_newline_is_still_one_field_on_the_wire() {
        let request = Request::SendLine {
            selector: SelectorSpec::All,
            line: "a\nb".to_string(),
        };
        let json = serde_json::to_string(&request).unwrap();
        // Escaped, not literal: the frame stays one JSON object. Refusing
        // it is the daemon's job, not serde's.
        assert!(json.contains(r#""line":"a\nb""#), "{json}");
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }

    /// The wire shape, pinned the way every other variant's is.
    #[test]
    fn set_sheep_env_batch_wire_v8() {
        let request = Request::SetSheepEnvBatch {
            name: "web".to_string(),
            entries: BTreeMap::from([("A".to_string(), EnvValue::from("1".to_string()))]),
            force: true,
            dry_run: false,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"set_sheep_env_batch","name":"web","entries":{"A":"1"},"force":true,"dry_run":false}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }

    /// Additive, so the floor does not move. Guards against a reflexive
    /// raise. The ceiling is pinned once, by `protocol::tests`, and moves
    /// for reasons unrelated to this variant.
    #[test]
    fn the_batch_variant_did_not_raise_the_floor() {
        assert_eq!(MIN_SUPPORTED, 8);
    }

    #[test]
    fn the_dog_verbs_serialize_snake_case_with_their_payloads_under_data() {
        assert_eq!(
            serde_json::to_string(&Request::DogConfig {
                name: "bark".to_string()
            })
            .unwrap(),
            r#"{"kind":"dog_config","name":"bark"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::DisableDog {
                name: "bark".to_string()
            })
            .unwrap(),
            r#"{"kind":"disable_dog","name":"bark"}"#
        );
        let section = Response::DogSection {
            toml: "port = 9615\n".to_string().into(),
        };
        let wire = r#"{"kind":"dog_section","data":{"toml":"port = 9615\n"}}"#;
        assert_eq!(serde_json::to_string(&section).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Response>(wire).unwrap(), section);
    }
}
