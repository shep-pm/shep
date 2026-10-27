use super::super::process::sample_info;
use super::super::{
    ActionOutcome, DogSource, EnvCopyChange, EnvCopyKind, ExitInfo, Lamb, LineOutcome, Reply,
    Request, RpcError, RpcErrorCode, SignalOutcome,
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

/// All three kinds, a reference shown as written and a value redacted.
#[test]
fn sheep_env_copied_response_wire_v12() {
    let response = Response::SheepEnvCopied {
        from: "web".to_string(),
        to: "api".to_string(),
        changes: vec![
            EnvCopyChange::new(
                "PADDOCK_KEY",
                EnvCopyKind::Added,
                None,
                Some("{{secret:PADDOCK_KEY}}".to_string()),
            )
            .sealed(11),
            EnvCopyChange::new(
                "PORT",
                EnvCopyKind::Changed,
                Some("<redacted>".to_string()),
                Some("<redacted>".to_string()),
            )
            .sealed(12),
            EnvCopyChange::new(
                "OLD",
                EnvCopyKind::Removed,
                Some("<redacted>".to_string()),
                None,
            )
            .sealed(13),
        ],
    };
    let json = serde_json::to_string(&response).unwrap();
    assert_eq!(
        json,
        r#"{"kind":"sheep_env_copied","data":{"from":"web","to":"api","changes":[{"key":"PADDOCK_KEY","kind":"added","old":null,"new":"{{secret:PADDOCK_KEY}}","seal":11},{"key":"PORT","kind":"changed","old":"<redacted>","new":"<redacted>","seal":12},{"key":"OLD","kind":"removed","old":"<redacted>","new":null,"seal":13}]}}"#
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
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel: false,
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
                dog: Some(DogSource::BuiltIn { grants: None }),
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
                    .lambs(Some(vec![
                        Lamb::new(4243, "node"),
                        Lamb::new(4244, "sh").with_label("worker 1"),
                    ]))
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
                        grants: None,
                        path: "/usr/local/bin/shep-log-rotate".to_string(),
                        channel: false,
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
                        grants: None,
                        path: "/usr/local/bin/shep-log-rotate".to_string(),
                        channel: false,
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
        // Two sheep, one table each, so this row proves a dog reads
        // more than one sheep's table from a single answer. The table
        // itself repeats `SetSheepDogSettings`'s own two keys, one
        // nested, so both rides through this enum agree on the shape.
        Reply {
            id: 43,
            result: Ok(Response::DogSheepSettings {
                tables: BTreeMap::from([
                    (
                        "web".to_string(),
                        DogTable::from(serde_json::Map::from_iter([
                            ("concurrency".to_string(), serde_json::json!(2)),
                            (
                                "hours".to_string(),
                                serde_json::json!({ "start": "09:00:00" }),
                            ),
                        ])),
                    ),
                    (
                        "worker".to_string(),
                        DogTable::from(serde_json::Map::from_iter([(
                            "concurrency".to_string(),
                            serde_json::json!(1),
                        )])),
                    ),
                ]),
            }),
        },
        Reply {
            id: 44,
            result: Ok(Response::SheepDogSettingsSet {
                name: "web".to_string(),
                dog: "jobs".to_string(),
            }),
        },
        Reply {
            id: 45,
            result: Ok(Response::LogTail(LogTail::Lines {
                lines: vec!["listening".to_string(), "ready".to_string()],
                truncated: true,
            })),
        },
        Reply {
            id: 46,
            result: Ok(Response::LogTail(LogTail::NoPath)),
        },
        Reply {
            id: 47,
            result: Ok(Response::LogTail(LogTail::Missing {
                path: "/var/log/web-err.log".to_string(),
            })),
        },
        Reply {
            id: 48,
            result: Ok(Response::LogTail(LogTail::Unreadable {
                path: "/var/log/web-err.log".to_string(),
                reason: "Is a directory (os error 21)".to_string(),
            })),
        },
        Reply {
            id: 49,
            result: Ok(Response::SheepDogTable {
                table: Some(DogTable::from(serde_json::Map::from_iter([(
                    "env".to_string(),
                    serde_json::json!({ "TOKEN": "resolved" }),
                )]))),
            }),
        },
        Reply {
            id: 50,
            result: Ok(Response::SheepDogTable { table: None }),
        },
        Reply {
            id: 51,
            result: Ok(Response::Reset(vec![sample_info()])),
        },
    ];
    insta::assert_json_snapshot!("reply_wire_v12", replies);
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

    let older = r#"{"kind":"sheep_field_set","data":{"name":"web","key":"script","pending":true}}"#;
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

/// One row's new status word must cost that row its status, not the
/// whole listing.
#[test]
fn a_flock_reply_with_one_row_of_unknown_status_still_decodes() {
    let known = sample_info();
    let mut odd = serde_json::to_value(ProcessInfo {
        id: 4,
        ..sample_info()
    })
    .unwrap();
    odd["status"] = serde_json::json!("invented_next_year");
    let wire = serde_json::json!({
        "kind": "flock",
        "data": [serde_json::to_value(&known).unwrap(), odd],
    });

    let decoded: Response = serde_json::from_value(wire).unwrap();

    assert_eq!(
        decoded,
        Response::Flock(vec![
            known,
            ProcessInfo {
                id: 4,
                status: ProcStatus::Unrecognized,
                ..sample_info()
            },
        ])
    );
}

/// IR-41: the reply prints how many keys the table holds and no value.
#[test]
fn sheep_dog_table_debug_prints_no_value() {
    const SENTINEL: &str = "sentinel-7f3a9c";
    let table = Response::SheepDogTable {
        table: Some(DogTable::from(serde_json::Map::from_iter([(
            "token".to_string(),
            serde_json::json!(SENTINEL),
        )]))),
    };
    let rendered = format!("{table:?}");
    assert_eq!(rendered, "SheepDogTable { table: Some(DogTable(<1 key>)) }");
    assert!(!rendered.contains(SENTINEL));
    assert_eq!(
        format!("{:?}", Response::SheepDogTable { table: None }),
        "SheepDogTable { table: None }"
    );
}
