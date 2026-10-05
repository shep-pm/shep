//! The request wire, pinned: one row per variant, and a protocol bump when a row changes.

use std::collections::BTreeMap;

use super::*;
use crate::config::{AppConfig, DeclaredApp, DogTable, ResetDepth};

/// A wire fixture row with no deadline, which every row but the first uses.
fn envelope(id: u64, body: Request) -> Envelope {
    Envelope {
        id,
        deadline_ms: None,
        body,
    }
}

#[test]
fn request_wire_snapshots() {
    let requests = vec![
        Envelope {
            id: 1,
            deadline_ms: Some(5000),
            body: Request::Ping,
        },
        envelope(2, Request::ListFlock),
        envelope(
            3,
            Request::Stop {
                selector: SelectorSpec::Name("web".to_string()),
            },
        ),
        envelope(
            4,
            Request::Start {
                apps: vec![AppConfig::minimal("web", "./srv")],
            },
        ),
        // `All` rather than a named sheep: the selector `shep reopen`
        // sends when given no argument.
        envelope(
            5,
            Request::Reopen {
                selector: SelectorSpec::All,
            },
        ),
        // The same selector as the row above, so the two log-plane rows
        // differ by their `kind` and by nothing else.
        envelope(
            6,
            Request::Flush {
                selector: SelectorSpec::All,
            },
        ),
        // The same selector as the `stop` row: `reload` under `stop`'s tag
        // shows up here as two identical objects.
        envelope(
            7,
            Request::Reload {
                selector: SelectorSpec::Name("web".to_string()),
            },
        ),
        // `action`/`params` match channel.rs's with-params fixture
        // verbatim, so a trigger reads the same at every hop.
        envelope(
            8,
            Request::Trigger {
                selector: SelectorSpec::Name("web".to_string()),
                action: "set-log-level".to_string(),
                params: Some("debug".to_string()),
            },
        ),
        // A fieldless verb: a bare `{"kind":"..."}` with no `selector` key.
        envelope(9, Request::SaveRoll),
        // Paired with the `save_roll` row: they differ by their `kind` alone.
        envelope(10, Request::Muster),
        // The three dog verbs. `enable_dog` and `disable_dog` differ by
        // their `kind` and by `source` alone.
        envelope(
            11,
            Request::DogConfig {
                name: "bark".to_string(),
            },
        ),
        envelope(
            12,
            Request::EnableDog {
                name: "metrics".to_string(),
                source: DogSource::BuiltIn,
            },
        ),
        envelope(
            13,
            Request::DisableDog {
                name: "metrics".to_string(),
            },
        ),
        // `Id`, `Regex` and `Fold` are three newtypes the wire tells apart
        // only by their `kind` tag: a `Fold` under `regex`'s tag turns
        // `shep restart fold:api` into a regex match.
        envelope(
            14,
            Request::Describe {
                selector: SelectorSpec::Id(7),
            },
        ),
        envelope(
            15,
            Request::Describe {
                selector: SelectorSpec::Regex("^web-".to_string()),
            },
        ),
        envelope(
            16,
            Request::Describe {
                selector: SelectorSpec::Fold("api".to_string()),
            },
        ),
        // `SIGHUP` rather than `SIGTERM`: the stop ladder already sends
        // TERM, so a TERM fixture could not tell the two frames apart.
        envelope(
            17,
            Request::Signal {
                selector: SelectorSpec::Name("web".to_string()),
                signal: "SIGHUP".to_string(),
            },
        ),
        // The one verb here whose body has no `selector` key.
        envelope(
            18,
            Request::Scale {
                name: "web".to_string(),
                count: 4,
            },
        ),
        // The line carries no terminator on the wire, since the shepherd
        // appends it.
        envelope(
            19,
            Request::SendLine {
                selector: SelectorSpec::All,
                line: "reload-config".to_string(),
            },
        ),
        // Both halves of the `Option` are pinned, a paint and a clear, so a
        // dog author does not have to guess the clear frame's shape.
        envelope(
            20,
            Request::SetSmit {
                sheep: "web".to_string(),
                smit: Some(
                    "\u{25b2} main@a1b2c3"
                        .parse()
                        .expect("the reference smit is valid"),
                ),
            },
        ),
        envelope(
            21,
            Request::SetSmit {
                sheep: "web".to_string(),
                smit: None,
            },
        ),
        // An empty `apps`: `start`'s row already pins the payload type, so
        // this row's own are the tag and the key the list travels under.
        envelope(22, Request::ConfigDrift { apps: Vec::new() }),
        // The only struct-shaped `SelectorSpec` variant, so the only place
        // `"kind":"instance"` and the `slot` key are pinned.
        envelope(
            23,
            Request::Restart {
                selector: SelectorSpec::Instance {
                    name: "web".to_string(),
                    slot: 2,
                },
            },
        ),
        // The one request an older daemon must never be sent: shep-cli
        // gates it on the daemon's crate version.
        envelope(24, Request::HandoverFitness),
        // The second request gated on the daemon's crate version.
        envelope(25, Request::DogStaleness),
        // The only request carrying a `DeclaredApp`: a merge keys on what a
        // document claimed. `declared_env` is non-empty to show it holds
        // env key names and no env value, and `reset` is pinned at a
        // non-default depth.
        envelope(
            26,
            Request::ApplyConfig {
                apps: vec![DeclaredApp {
                    config: AppConfig::minimal("web", "./srv"),
                    declared: ["name", "script"]
                        .iter()
                        .map(|k| (*k).to_string())
                        .collect(),
                    declared_env: ["DATABASE_URL"].iter().map(|k| (*k).to_string()).collect(),
                }],
                reset: ResetDepth::Policy,
            },
        ),
        // The same app as the `start` row above: the two differ by their
        // `kind` alone, so a mis-tagged `add` shows up as two identical
        // objects.
        envelope(
            27,
            Request::Add {
                apps: vec![AppConfig::minimal("web", "./srv")],
            },
        ),
        // The four config-pane requests. `SheepConfig` takes a name
        // rather than a selector, like `Scale` and `SetSmit` above and
        // for their reason: a pane edits one sheep.
        envelope(
            28,
            Request::SheepConfig {
                name: "web".to_string(),
            },
        ),
        // `value` is pinned as `Some`, because the `None` spelling is
        // what removes the key, and a reader that guessed the two apart
        // wrongly would delete an operator's env instead of setting it.
        // The value is a placeholder, not a secret: this is the one
        // request in the enum that carries an env value at all, and it
        // travels in one direction only, nothing ever reads it back.
        envelope(
            29,
            Request::SetSheepEnv {
                name: "web".to_string(),
                key: "DATABASE_URL".to_string(),
                value: Some("postgres://localhost/app".to_string().into()),
            },
        ),
        // `SetSheepEnv`'s twin for everything that is not `env`, and
        // pinned beside it: the two are one letter apart in the tag and
        // a reader that crossed them would write a config field into an
        // env map. `value` is a bare JSON value rather than a string,
        // which is the half a hand-written reader gets wrong: an
        // integer field is an integer here, not `"32"`.
        envelope(
            30,
            Request::SetSheepField {
                name: "web".to_string(),
                key: "max_restarts".to_string(),
                value: serde_json::json!(32),
            },
        ),
        // The second request carrying a `DogSectionToml`, and pinned
        // beside its reader: `DogConfig` asks for a section and this
        // writes one back, so the two have to agree about the shape a
        // section takes on the wire.
        envelope(
            31,
            Request::SetDogConfig {
                name: "bark".to_string(),
                toml: "debounce = \"30s\"\n".to_string().into(),
            },
        ),
        // The one request a provider dog sends, and the row that pins
        // what `EnvValue` costs the wire: `entries` is a plain object
        // of strings, so a dog written against this fixture in another
        // language needs no newtype of its own.
        envelope(
            32,
            Request::PutSecrets {
                namespace: "vercel".to_string(),
                environment: "production".to_string(),
                entries: BTreeMap::from([(
                    "API_KEY".to_string(),
                    EnvValue::from("sk_live_placeholder".to_string()),
                )]),
            },
        ),
        // `SetSheepEnvBatch`'s own doc comment calls this the densest
        // run of secrets on the wire, so it gets two entries rather
        // than one: a single-entry map would not distinguish an object
        // from a map with one key. `force` and `dry_run` are both
        // pinned away from their default so a silent default flip on
        // either field shows up here.
        envelope(
            33,
            Request::SetSheepEnvBatch {
                name: "web".to_string(),
                entries: BTreeMap::from([
                    (
                        "DATABASE_URL".to_string(),
                        EnvValue::from("postgres://localhost/app".to_string()),
                    ),
                    (
                        "API_KEY".to_string(),
                        EnvValue::from("sk_live_placeholder".to_string()),
                    ),
                ]),
                force: true,
                dry_run: true,
            },
        ),
        envelope(34, Request::HostUsage),
        envelope(
            35,
            Request::DogSheepSettings {
                dog: "jobs".to_string(),
            },
        ),
        // The table carries two keys, one nested, so a reader sees a
        // table survives the hop through this enum without flattening.
        envelope(
            36,
            Request::SetSheepDogSettings {
                name: "web".to_string(),
                dog: "jobs".to_string(),
                table: Some(DogTable::from(serde_json::Map::from_iter([
                    ("concurrency".to_string(), serde_json::json!(2)),
                    (
                        "hours".to_string(),
                        serde_json::json!({ "start": "09:00:00" }),
                    ),
                ]))),
            },
        ),
        envelope(
            37,
            Request::SetSheepDogSettings {
                name: "web".to_string(),
                dog: "jobs".to_string(),
                table: None,
            },
        ),
    ];
    insta::assert_json_snapshot!("request_wire_v11", requests);
}
