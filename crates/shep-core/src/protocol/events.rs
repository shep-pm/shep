//! Bus events broadcast to subscribed clients

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::protocol::request::{ProcessInfo, Settled};
use crate::protocol::{ChildMessage, QuestionId};

/// What happened to a sheep
// wire format: changing existing variants is a breaking change
//
// A new variant is free for a subscriber built from this point on: an
// unrecognized string decodes as `Unrecognized` instead of failing the
// whole frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessEventKind {
    /// Spawn initiated
    Start,
    /// Became ready/online
    Online,
    /// Process exited
    Exit,
    /// Restart initiated
    Restart,
    /// A reload is replacing this instance: its replacement has been spawned
    /// into the same instance slot, and this one will be asked to go once
    /// that replacement is serving
    Reload,
    /// This instance has replaced the one it was spawned to drain; that one
    /// is gone.
    Reloaded,
    /// A reload gave up, so the instances it had not reached are left alone
    ///
    /// The instance named is whichever one the abandonment left holding the
    /// slot: still the app's live instance if the reload gave up before
    /// replacing it, or the replacement if that one went down instead.
    /// `info` reflects that instance's state at event time; read
    /// `info.status` rather than assume it is live.
    ReloadAbandoned,
    /// Stopped by request
    Stop,
    /// Deregistered
    Delete,
    /// Restart budget exhausted
    Errored,
    /// An event kind this build has not been taught.
    ///
    /// Only ever produced by decoding: an unrecognized string falls through
    /// to this variant via `#[serde(other)]` instead of failing the whole
    /// frame. Nothing constructs one to send, and `bark`'s `is_known_kind`
    /// refuses the spelling before a rule can name it. That is a call-site
    /// invariant rather than a type-level one: `#[serde(other)]` governs
    /// decoding only, so serializing this would emit `"unrecognized"`.
    #[serde(other)]
    Unrecognized,
}

/// One event on the daemon bus
///
/// Adjacently tagged: `event` discriminator, `data` wrapper. Subscription
/// topics are the dotted strings from [`BusEvent::topic`] (`process.exit`,
/// `log.out`, `daemon.*`); the daemon's filter globs against them.
// wire format: changing existing variants is a breaking change
//
// `large_enum_variant` allowed: boxing `Process` would break every match on
// it, for no benefit since an event is serialized immediately.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// Adjacently tagged, not internally: `Process`'s own `event` field would
// collide with an internal tag named `event`, and serde_derive refuses
// that.
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BusEvent {
    /// Lifecycle event for one sheep
    Process {
        /// What happened
        event: ProcessEventKind,
        /// Sheep snapshot at event time
        info: ProcessInfo,
        /// True when a user action caused it
        manually: bool,
        /// Unix millis
        at_ms: u64,
    },
    /// One stdout line from a sheep
    LogOut {
        /// Sheep id
        id: u32,
        /// The line (no trailing newline)
        line: String,
    },
    /// One stderr line from a sheep
    LogErr {
        /// Sheep id
        id: u32,
        /// The line
        line: String,
    },
    /// One message a sheep wrote on its shepherd channel (fd 3).
    ///
    /// Child -> shepherd only: the shepherd's own writes (a shutdown
    /// message, a dispatched action) are reported elsewhere, by
    /// `process.stop` and `Response::Triggered`.
    ///
    /// `message` is the app's own text, whole and unredacted. The daemon
    /// adds nothing of its own; app-provided text must be safe for every
    /// subscriber, since it is broadcast verbatim.
    Channel {
        /// The sheep that wrote it.
        id: u32,
        /// The message, exactly as it came off fd 3.
        message: ChildMessage,
    },
    /// A question a sheep had open is no longer open. Published under
    /// `question.settled`.
    ///
    /// Carries the question's id and how it ended, never its text or the
    /// answer: the bus is a broadcast, and an answer can be a secret.
    QuestionSettled {
        /// The sheep that asked.
        id: u32,
        /// Name of the sheep that asked.
        name: String,
        /// The question that settled.
        question: QuestionId,
        /// How it stopped being open.
        settled: Settled,
        /// Unix millis
        at_ms: u64,
    },
    /// The bounded queue dropped this many events for this subscriber
    Dropped {
        /// Dropped-event count since last notice
        count: u64,
    },
    /// Daemon is shutting down
    DaemonShutdown,
    /// A dog's section in `dogs.toml` changed. Published under
    /// `config.dog.<name>`, so a dog subscribes to its own name and hears
    /// nobody else's.
    ///
    /// Carries only the dog's name, nothing else: the bus is a broadcast,
    /// and a `[bark]` section can hold a webhook URL as a bearer credential
    /// (why [`DogSectionToml`] redacts its own `Debug`). A dog that wants
    /// the values re-asks with
    /// [`Request::DogConfig`](crate::protocol::Request::DogConfig), which
    /// answers only that dog's own section.
    ///
    /// [`DogSectionToml`]: crate::protocol::DogSectionToml
    DogConfigChanged {
        /// The dog whose section changed.
        dog: String,
    },
    /// A dog's `[app.dogs.<name>]` table changed on one sheep. Published
    /// under `config.sheep.<dog>`, so a dog subscribes to its own name and
    /// hears nobody else's, the way [`Self::DogConfigChanged`] does for
    /// `dogs.toml`.
    ///
    /// Fires once per dog and sheep for a change that adds, removes or
    /// edits that sheep's table for `dog`: a name gaining its first table,
    /// the last instance of such a name being deleted, or a load,
    /// `Request::SetSheepDogSettings` or `--reset` changing one. Never for
    /// a handover, since that changes no answer.
    ///
    /// Carries only the sheep's name and the dog's: a table can hold a
    /// credential, the same reason [`Self::DogConfigChanged`] carries only
    /// the dog's own name. A dog that wants the values re-asks with
    /// [`Request::DogSheepSettings`](crate::protocol::Request::DogSheepSettings),
    /// which answers with that dog's tables.
    DogSheepSettingsChanged {
        /// The dog whose table changed.
        dog: String,
        /// The sheep whose table changed.
        sheep: String,
    },
}

impl BusEvent {
    /// The dotted subscription topic for this event (spec §6 grammar)
    ///
    /// A [`Cow`] rather than a `&'static str`, because two topics are not
    /// fixed: [`Self::DogConfigChanged`] and [`Self::DogSheepSettingsChanged`]
    /// name their dog in the topic itself, which is what lets a dog
    /// subscribe to its own config and hear nobody else's. Every other
    /// variant is still a borrowed literal and allocates nothing.
    #[must_use]
    pub fn topic(&self) -> Cow<'static, str> {
        let fixed = match self {
            Self::Process { event, .. } => match event {
                ProcessEventKind::Start => "process.start",
                ProcessEventKind::Online => "process.online",
                ProcessEventKind::Exit => "process.exit",
                ProcessEventKind::Restart => "process.restart",
                ProcessEventKind::Reload => "process.reload",
                ProcessEventKind::Reloaded => "process.reloaded",
                ProcessEventKind::ReloadAbandoned => "process.reload_abandoned",
                ProcessEventKind::Stop => "process.stop",
                ProcessEventKind::Delete => "process.delete",
                ProcessEventKind::Errored => "process.errored",
                // Decode-only, never emitted by this build. Given its own
                // topic rather than acted on: a subscriber sees it pass
                // through like any other kind it does not specifically
                // filter for, instead of the daemon treating it as a real
                // lifecycle transition.
                ProcessEventKind::Unrecognized => "process.unrecognized",
            },
            Self::LogOut { .. } => "log.out",
            Self::LogErr { .. } => "log.err",
            // Total match over `ChildMessage`: a new kind on fd 3 fails
            // to compile here until its topic is decided.
            Self::Channel { message, .. } => match message {
                ChildMessage::Ready => "channel.ready",
                ChildMessage::Metric { .. } => "channel.metric",
                ChildMessage::ActionReply { .. } => "channel.action_reply",
                ChildMessage::LambLabel { .. } => "channel.lamb_label",
                ChildMessage::Ask { .. } => "channel.ask",
                ChildMessage::Withdraw { .. } => "channel.withdraw",
            },
            Self::QuestionSettled { .. } => "question.settled",
            Self::Dropped { .. } => "daemon.dropped",
            Self::DaemonShutdown => "daemon.shutdown",
            // Built rather than named. `config.dog.` is the prefix a
            // subscriber globs on; the dog's own name is the last segment,
            // so `config.dog.bark` reaches one dog and `config.*` reaches
            // all of them.
            Self::DogConfigChanged { dog } => return Cow::Owned(format!("config.dog.{dog}")),
            // Named by the dog, like `DogConfigChanged` above: the sheep is
            // the payload, not the topic, so a dog subscribes once for
            // every sheep carrying its table.
            Self::DogSheepSettingsChanged { dog, .. } => {
                return Cow::Owned(format!("config.sheep.{dog}"));
            }
        };
        Cow::Borrowed(fixed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::request::{ExitInfo, ProcessInfo};
    use crate::protocol::{LambLabel, QuestionId, QuestionText, Takes};
    use crate::status::ProcStatus;

    /// The comment above this enum said a new variant is not free because
    /// there is no fallback. This is the fallback.
    #[test]
    fn an_unknown_process_event_decodes_as_unrecognized() {
        assert_eq!(
            serde_json::from_str::<ProcessEventKind>(r#""invented_next_year""#).unwrap(),
            ProcessEventKind::Unrecognized
        );
    }

    #[test]
    fn every_known_process_event_still_round_trips() {
        for kind in [
            ProcessEventKind::Start,
            ProcessEventKind::Online,
            ProcessEventKind::Exit,
            ProcessEventKind::Restart,
            ProcessEventKind::Reload,
            ProcessEventKind::Reloaded,
            ProcessEventKind::ReloadAbandoned,
            ProcessEventKind::Stop,
            ProcessEventKind::Delete,
            ProcessEventKind::Errored,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(
                serde_json::from_str::<ProcessEventKind>(&json).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn a_non_string_process_event_is_still_an_error() {
        assert!(serde_json::from_str::<ProcessEventKind>("42").is_err());
    }

    #[test]
    fn bus_event_wire_snapshots() {
        let mut events = vec![
            BusEvent::Process {
                event: ProcessEventKind::Exit,
                info: ProcessInfo {
                    id: 3,
                    name: "web".to_string(),
                    status: ProcStatus::WaitingRestart,
                    pid: None,
                    restarts: 2,
                    uptime_ms: 500,
                    fold: None,
                    depends_on: Vec::new(),
                    out_file: Some("/home/ada/.shep/logs/web-0-out.log".to_string()),
                    err_file: Some("/home/ada/.shep/logs/web-0-err.log".to_string()),
                    // A bus event is built from the actor's own snapshot,
                    // which never carries a resource reading.
                    cpu_percent: None,
                    memory_bytes: None,
                    cpu_ms: None,
                    dog: None,
                    lambs: None,
                    questions: None,
                    // `handle_exited` sets `last_exit` before deciding what
                    // to do with the exit, so this `Exit` row carries the
                    // outcome it announces.
                    last_exit: Some(ExitInfo {
                        code: Some(1),
                        signal: None,
                    }),
                    // A non-ASCII marker on purpose: this snapshot is what
                    // pins the encoding a subscriber reads, and a smit is
                    // the one field on this row a third party writes.
                    smit: Some("\u{25b2} main@a1b2c3".to_string()),
                    instance: None,
                    handshook: None,
                    dog_stale: None,
                    pending: None,
                    overridden: None,
                    max_memory: None,
                    level_rules: Vec::new(),
                    reload_deadline_ms: None,
                },
                manually: false,
                at_ms: 1_700_000_000_000,
            },
            BusEvent::LogOut {
                id: 3,
                line: "listening on :8080".to_string(),
            },
            BusEvent::Dropped { count: 17 },
            // The row above pins `instance`'s absent shape; every lifecycle
            // row below reuses it via `sample`. This is the only place the
            // present shape (a live slot on a scaled app) is on the wire.
            BusEvent::Process {
                event: ProcessEventKind::Online,
                info: ProcessInfo::builder(4, "web", ProcStatus::Online)
                    .pid(Some(5150))
                    .instance(Some(2))
                    .build(),
                manually: false,
                at_ms: 1_700_000_000_000,
            },
        ];

        // One identical `info` reused below, so these rows differ only by
        // their `event` tag: a variant rename changes the wire string
        // silently otherwise.
        let sample = ProcessInfo::builder(3, "web", ProcStatus::WaitingRestart)
            .restarts(2)
            .uptime_ms(500)
            .out_file(Some("/home/ada/.shep/logs/web-0-out.log".to_string()))
            .err_file(Some("/home/ada/.shep/logs/web-0-err.log".to_string()))
            // Reused below for `Stop` and `Delete` too: both still carry the
            // exit that produced them.
            .last_exit(Some(ExitInfo {
                code: Some(1),
                signal: None,
            }))
            .build();

        let lifecycle = [
            ProcessEventKind::Start,
            ProcessEventKind::Online,
            ProcessEventKind::Restart,
            ProcessEventKind::Stop,
            ProcessEventKind::Delete,
            ProcessEventKind::Errored,
        ]
        .map(|event| BusEvent::Process {
            event,
            info: sample.clone(),
            manually: false,
            at_ms: 1_700_000_000_000,
        });

        events.extend(lifecycle);

        // The adjacent-tagged shape nests the message's own `kind` inside
        // `data`, next to `id`: easy to get wrong by hand.
        events.extend([
            BusEvent::Channel {
                id: 3,
                message: ChildMessage::Ready,
            },
            BusEvent::Channel {
                id: 3,
                message: ChildMessage::Metric {
                    name: "rps".to_string(),
                    value: 42.0,
                },
            },
            BusEvent::Channel {
                id: 3,
                message: ChildMessage::ActionReply {
                    action: "gc".to_string(),
                    body: "freed 12MB".to_string(),
                    id: Some(7),
                },
            },
        ]);

        // Last, so every row above keeps its index. The one topic that is
        // not a fixed string, and the one frame a dog subscribes to on its
        // own name: an operator's `dogs.toml` edit reaches a running dog
        // through this shape or through nothing.
        events.push(BusEvent::DogConfigChanged {
            dog: "bark".to_string(),
        });
        // The second event whose topic is built rather than named, and the
        // one that carries a sheep's name beside the dog's.
        events.push(BusEvent::DogSheepSettingsChanged {
            dog: "jobs".to_string(),
            sheep: "web".to_string(),
        });
        // `settled` nests its own `kind` inside `data`, beside the
        // question's id.
        events.push(BusEvent::QuestionSettled {
            id: 3,
            name: "web".to_string(),
            question: QuestionId::new("deploy-1").unwrap(),
            settled: Settled::Answered {
                via: Some("discord".to_string()),
                who: Some("ops".to_string()),
            },
            at_ms: 1_700_000_000_000,
        });

        insta::assert_json_snapshot!("bus_event_wire_v11", events);
    }

    #[test]
    fn a_settled_question_has_its_topic_and_round_trips() {
        let event = BusEvent::QuestionSettled {
            id: 3,
            name: "web".to_string(),
            question: QuestionId::new("deploy-1").unwrap(),
            settled: Settled::Answered {
                via: Some("discord".to_string()),
                who: None,
            },
            at_ms: 1_700_000_000_000,
        };
        assert_eq!(event.topic(), "question.settled");
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<BusEvent>(&json).unwrap(), event);
    }

    #[test]
    fn topics_follow_the_dotted_grammar() {
        // spec §6: process.* / log.out / log.err / daemon.*
        let e = BusEvent::LogOut {
            id: 1,
            line: String::new(),
        };
        assert_eq!(e.topic(), "log.out");
        assert_eq!(BusEvent::DaemonShutdown.topic(), "daemon.shutdown");
    }

    /// The three kinds a reload reports itself with, pinned as topic
    /// strings and wire strings: a reload's reply is an acceptance, so
    /// these frames are the only place a client learns how it went.
    #[test]
    fn a_reload_reports_itself_under_three_topics() {
        for (kind, topic, wire) in [
            (ProcessEventKind::Reload, "process.reload", "\"reload\""),
            (
                ProcessEventKind::Reloaded,
                "process.reloaded",
                "\"reloaded\"",
            ),
            (
                ProcessEventKind::ReloadAbandoned,
                "process.reload_abandoned",
                "\"reload_abandoned\"",
            ),
        ] {
            let event = BusEvent::Process {
                event: kind,
                // Scaffolding: `topic()` reads only `event`, and nothing
                // here is asserted. The builder says so; the exhaustive
                // literal in `bus_event_wire_snapshots` says the opposite
                // about its own row, deliberately.
                info: ProcessInfo::builder(3, "web", ProcStatus::Stopping)
                    .pid(Some(4242))
                    .build(),
                manually: true,
                at_ms: 0,
            };
            assert_eq!(event.topic(), topic, "{kind:?}");
            assert_eq!(serde_json::to_string(&kind).unwrap(), wire, "{kind:?}");
        }
    }

    #[test]
    fn v1_bus_event_fixture_still_deserializes() {
        // Adjacent-tagged shape pinned as a byte fixture.
        let fixture = r#"{"event":"log_out","data":{"id":3,"line":"ready"}}"#;
        let ev: BusEvent = serde_json::from_str(fixture).unwrap();
        assert!(matches!(ev, BusEvent::LogOut { id: 3, .. }));
    }

    /// The exact topic strings are the contract, not just the `channel.*`
    /// prefix.
    #[test]
    fn every_shepherd_channel_message_has_its_own_topic() {
        for (message, topic) in [
            (ChildMessage::Ready, "channel.ready"),
            (
                ChildMessage::Metric {
                    name: "rps".to_string(),
                    value: 42.0,
                },
                "channel.metric",
            ),
            (
                ChildMessage::ActionReply {
                    action: "gc".to_string(),
                    body: "ok".to_string(),
                    id: Some(7),
                },
                "channel.action_reply",
            ),
            (
                ChildMessage::LambLabel {
                    pid: 4312,
                    label: LambLabel::new("worker 1").unwrap(),
                },
                "channel.lamb_label",
            ),
            (
                ChildMessage::Ask {
                    question: QuestionId::new("q1").unwrap(),
                    text: QuestionText::new("Merge?").unwrap(),
                    takes: Takes::YesNo,
                },
                "channel.ask",
            ),
            (
                ChildMessage::Withdraw {
                    question: QuestionId::new("q1").unwrap(),
                },
                "channel.withdraw",
            ),
        ] {
            let event = BusEvent::Channel {
                id: 3,
                message: message.clone(),
            };
            assert_eq!(event.topic(), topic, "{message:?}");
        }
    }

    /// `channel.*` is the only pattern anyone subscribes with; a topic that
    /// drifts out from under it becomes unreachable.
    #[test]
    fn the_channel_glob_reaches_every_channel_topic() {
        for message in [
            ChildMessage::Ready,
            ChildMessage::Metric {
                name: "rps".to_string(),
                value: 1.0,
            },
            ChildMessage::ActionReply {
                action: "gc".to_string(),
                body: String::new(),
                id: None,
            },
            ChildMessage::LambLabel {
                pid: 1,
                label: LambLabel::new("").unwrap(),
            },
            ChildMessage::Ask {
                question: QuestionId::new("q1").unwrap(),
                text: QuestionText::new("Merge?").unwrap(),
                takes: Takes::Text,
            },
            ChildMessage::Withdraw {
                question: QuestionId::new("q1").unwrap(),
            },
        ] {
            let topic = BusEvent::Channel { id: 1, message }.topic();
            assert!(
                topic.starts_with("channel."),
                "`{topic}` is not under the channel.* glob"
            );
        }
    }

    /// The message carries verbatim: nothing on this wire is a credential.
    #[test]
    fn a_channel_event_carries_the_message_verbatim() {
        let event = BusEvent::Channel {
            id: 3,
            message: ChildMessage::ActionReply {
                action: "gc".to_string(),
                body: "freed 12MB".to_string(),
                id: Some(7),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("freed 12MB"), "{json}");
        assert_eq!(serde_json::from_str::<BusEvent>(&json).unwrap(), event);
    }

    /// The topic is the whole of what a dog subscribes with; a name that
    /// misses it leaves the dog listening to nothing.
    #[test]
    fn a_dog_config_event_names_the_dog_in_its_topic() {
        for dog in ["bark", "metrics", "otel-shipper"] {
            let event = BusEvent::DogConfigChanged {
                dog: dog.to_string(),
            };
            assert_eq!(event.topic(), format!("config.dog.{dog}"));
        }
    }

    /// A value here would put another dog's webhook credential in front of
    /// every subscriber on `config.*`.
    #[test]
    fn a_dog_config_event_carries_the_name_and_nothing_else() {
        let event = BusEvent::DogConfigChanged {
            dog: "bark".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"dog_config_changed","data":{"dog":"bark"}}"#
        );
        assert_eq!(serde_json::from_str::<BusEvent>(&json).unwrap(), event);
    }

    /// The topic names the dog, not the sheep, so one dog's subscription
    /// reaches every sheep carrying its table.
    #[test]
    fn a_dog_sheep_settings_event_names_the_dog_in_its_topic() {
        for dog in ["jobs", "metrics"] {
            let event = BusEvent::DogSheepSettingsChanged {
                dog: dog.to_string(),
                sheep: "web".to_string(),
            };
            assert_eq!(event.topic(), format!("config.sheep.{dog}"));
        }
    }

    /// `config.*` still reaches this event, beside `config.dog.*`: a
    /// dashboard watching every dog's config must not lose this topic to a
    /// glob that only matched the older shape.
    #[test]
    fn a_dog_sheep_settings_topic_is_still_under_the_config_glob() {
        let event = BusEvent::DogSheepSettingsChanged {
            dog: "jobs".to_string(),
            sheep: "web".to_string(),
        };
        assert!(event.topic().starts_with("config."));
    }

    /// The sheep rides in the payload, not the topic: a dog subscribes once
    /// on its own name and hears about every sheep carrying its table.
    #[test]
    fn a_dog_sheep_settings_event_carries_the_sheep_and_the_dog() {
        let event = BusEvent::DogSheepSettingsChanged {
            dog: "jobs".to_string(),
            sheep: "web".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"dog_sheep_settings_changed","data":{"dog":"jobs","sheep":"web"}}"#
        );
        assert_eq!(serde_json::from_str::<BusEvent>(&json).unwrap(), event);
    }
}
