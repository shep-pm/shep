//! A dog's defaults write as the schema judges it over the sheep it reaches:
//! each failing sheep is named, and a default every sheep shadows is judged by
//! its own value.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json, json};
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::config::DogTable;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{Request, Response};

use super::flow::run;
use super::plan::{plan_dog_defaults, schema_refusal};
use super::{Flags, Plan, Verb};
use crate::cli::Format;
use crate::exit::ExitCode;
use crate::output::Streams;

/// The per-sheep schema a dog publishes: `keep` has a maximum of 4.
fn schema() -> Json {
    json!({
        "type": "object",
        "properties": { "keep": { "type": "integer", "minimum": 1, "maximum": 4 } },
    })
}

/// `keep` is bounded at 4, so a default of 9 breaks it on every sheep that
/// does not hold its own `keep`: `a` and `c` are named, `b` is not.
fn default_plan(keep: i64) -> Plan {
    let sheep = BTreeMap::from([
        ("a".to_string(), DogTable::from(Map::new())),
        (
            "b".to_string(),
            DogTable::from(json!({ "keep": 2 }).as_object().unwrap().clone()),
        ),
        (
            "c".to_string(),
            DogTable::from(json!({ "merge": "ask" }).as_object().unwrap().clone()),
        ),
    ]);
    default_plan_over(keep, sheep)
}

/// [`default_plan`] over the given sheep.
fn default_plan_over(keep: i64, sheep: BTreeMap<String, DogTable>) -> Plan {
    let plan = plan_dog_defaults(
        "kelpie",
        &["keep".to_string()],
        Json::Object(Map::new()),
        sheep,
        &json!(keep),
    )
    .unwrap();
    Plan {
        schema: Some(schema()),
        ..plan
    }
}

/// A default some sheep's layered table would break is refused naming each of
/// them and the rule, and the shepherd is sent nothing.
#[tokio::test]
async fn a_refused_default_names_every_failing_sheep_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_request| Ok(Response::Pong))
            .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        run(
            &client,
            &mut streams,
            &paths,
            "kelpie",
            "defaults.keep",
            &default_plan(9),
            Flags {
                verb: Verb::Set,
                yes: true,
                dry_run: false,
            },
        )
        .await
    };

    let said = String::from_utf8_lossy(&err).to_string();
    assert_eq!(code, ExitCode::InvalidConfig, "{said}");
    assert!(said.contains("`kelpie.defaults.keep`"), "{said}");
    assert!(said.contains("2 of the sheep it reaches"), "{said}");
    assert!(said.contains("`a` would hold"), "{said}");
    assert!(said.contains("`c` would hold"), "{said}");
    assert!(!said.contains("`b`"), "b holds its own `keep`: {said}");
    assert!(
        envelopes.try_recv().is_err(),
        "a refused default sends nothing"
    );
}

/// A default is judged by its own value even where every sheep holds its own
/// `keep`: a reset of one would expose it.
#[tokio::test]
async fn a_refused_default_every_sheep_shadows_is_reported_once_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |_request| Ok(Response::Pong))
            .await;
    let own = |keep: i64| DogTable::from(json!({ "keep": keep }).as_object().unwrap().clone());
    let plan = default_plan_over(
        9,
        BTreeMap::from([("a".to_string(), own(2)), ("b".to_string(), own(3))]),
    );

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        run(
            &client,
            &mut streams,
            &paths,
            "kelpie",
            "defaults.keep",
            &plan,
            Flags {
                verb: Verb::Set,
                yes: true,
                dry_run: false,
            },
        )
        .await
    };

    let said = String::from_utf8_lossy(&err).to_string();
    assert_eq!(code, ExitCode::InvalidConfig, "{said}");
    assert!(said.contains("the default itself holds"), "{said}");
    assert!(
        !said.contains("would hold"),
        "no sheep reads it today: {said}"
    );
    assert!(
        envelopes.try_recv().is_err(),
        "a refused default sends nothing"
    );
}

/// The control: the default every sheep it reaches takes goes out whole, as
/// `SetDogDefaults`, after the holder is read once more.
#[tokio::test]
async fn a_default_every_sheep_takes_is_sent_whole() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |request| match request {
            Request::DogDefaults { .. } => Ok(Response::DogDefaults { table: None }),
            Request::SetDogDefaults { dog, .. } => {
                Ok(Response::DogDefaultsSet { dog: dog.clone() })
            }
            _other => Ok(Response::Pong),
        })
        .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        run(
            &client,
            &mut streams,
            &paths,
            "kelpie",
            "defaults.keep",
            &default_plan(3),
            Flags {
                verb: Verb::Set,
                yes: true,
                dry_run: false,
            },
        )
        .await
    };

    let said = String::from_utf8_lossy(&err).to_string();
    assert_eq!(code, ExitCode::Success, "{said}");
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        sent.push(envelope.body);
    }
    let Some(Request::SetDogDefaults { dog, table }) = sent.last() else {
        panic!("the write went out last: {sent:?}");
    };
    assert_eq!(dog, "kelpie");
    assert_eq!(
        table.as_ref().map(|table| table.as_map().clone()),
        json!({ "keep": 3 }).as_object().cloned()
    );
}

/// A dog that published no per-sheep schema is checked against nothing, so a
/// default no schema can judge is not refused.
#[test]
fn a_default_for_a_dog_with_no_sheep_schema_is_checked_against_nothing() {
    let plan = Plan {
        schema: None,
        ..default_plan(9)
    };
    assert!(schema_refusal(&plan, "kelpie", "defaults.keep").is_none());
}
