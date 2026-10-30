//! A dog's defaults write, as the dog and the schema each judge it: the dog is
//! asked once per sheep the default reaches, and a default replaced whole is
//! answerable only for the leaves it holds.

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json, json};
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_core::config::DogTable;
use shep_core::dogs::validate::Proposed;
use shep_core::protocol::{ActionOutcome, ActionReply, Request, Response};

use super::flow::run;
use super::plan::{plan_dog_defaults, schema_refusal};
use super::{Flags, Plan, Verb};
use crate::cli::Format;
use crate::dog_settings::validate_tests::{home_with, validating};
use crate::exit::ExitCode;
use crate::output::Streams;

/// The per-sheep schema a dog publishes: `keep` has a maximum of 4.
fn schema() -> Json {
    json!({
        "type": "object",
        "properties": { "keep": { "type": "integer", "minimum": 1, "maximum": 4 } },
    })
}

fn table(value: Json) -> DogTable {
    DogTable::from(value.as_object().cloned().unwrap_or_default())
}

/// The plan for `keys` set to `value` over an empty default, reaching `sheep`.
fn plan(keys: &[&str], value: Json, sheep: &[(&str, Json)]) -> Plan {
    let keys: Vec<String> = keys.iter().map(|key| (*key).to_string()).collect();
    let sheep: BTreeMap<String, DogTable> = sheep
        .iter()
        .map(|(name, own)| ((*name).to_string(), table(own.clone())))
        .collect();
    let plan = plan_dog_defaults("kelpie", &keys, Json::Object(Map::new()), sheep, &value).unwrap();
    Plan {
        schema: Some(schema()),
        ..plan
    }
}

/// A dog that declared `validate` and turns down `model = "x"` wherever it
/// reads it, so `a`, which holds a `model` of its own, is the one it accepts.
#[tokio::test]
async fn a_dog_that_turns_a_value_down_is_asked_for_every_sheep_that_would_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    let (client, mut envelopes) =
        fake_client_answering_refusals(&control_address(dir.path()), |request| match request {
            Request::DogDefaults { .. } => Ok(Response::DogDefaults { table: None }),
            Request::Trigger {
                params: Some(params),
                ..
            } => {
                let proposed = Proposed::parse(params).expect("a table and a sheep");
                let refused = proposed.table().get("model") == Some(&json!("x"));
                let body = if refused {
                    r#"{"accept":false,"reason":"model `x` is not one this checkout runs"}"#
                } else {
                    r#"{"accept":true}"#
                };
                Ok(Response::Triggered(vec![ActionReply {
                    id: 7,
                    name: "kelpie".to_string(),
                    outcome: ActionOutcome::Replied {
                        body: body.to_string(),
                    },
                }]))
            }
            _other => Ok(Response::Pong),
        })
        .await;
    let plan = plan(
        &["model"],
        json!("x"),
        &[("a", json!({ "model": "opus" })), ("b", json!({}))],
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
            "defaults.model",
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
    assert_ne!(code, ExitCode::Success, "{said}");
    assert!(
        said.contains("`kelpie` refused `kelpie`'s defaults as `b` would read them"),
        "{said}"
    );
    assert!(said.contains("model `x` is not one"), "{said}");
    let mut requests = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        requests.push(envelope.body);
    }
    let asked = requests
        .iter()
        .filter(|request| matches!(request, Request::Trigger { .. }))
        .count();
    assert_eq!(asked, 2, "one ask per sheep, in name order: {requests:?}");
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, Request::SetDogDefaults { .. })),
        "a refused default is not sent: {requests:?}"
    );
}

/// A default replaced whole is answerable for what it holds. `web`'s own
/// `keep = 9` is out of the schema, and a default that never mentions `keep`
/// did not put it there.
#[test]
fn a_default_replaced_whole_is_not_blamed_for_a_value_a_sheep_holds_itself() {
    let own = [("web", json!({ "keep": 9 }))];

    let replaced = plan(&[], json!({ "model": "opus" }), &own);
    assert!(
        schema_refusal(&replaced, "kelpie", "defaults").is_none(),
        "the default holds no `keep`"
    );

    let cleared = plan(&[], json!({}), &own);
    assert!(
        schema_refusal(&cleared, "kelpie", "defaults").is_none(),
        "clearing a default puts no value anywhere"
    );
}

/// The same whole-table write is refused for a leaf it does hold, and names
/// the sheep that would read it.
#[test]
fn a_default_replaced_whole_is_refused_for_a_leaf_it_holds() {
    let plan = plan(&[], json!({ "keep": 9 }), &[("web", json!({}))]);

    let refusal = schema_refusal(&plan, "kelpie", "defaults").expect("keep 9 is over the maximum");

    assert!(
        refusal.to_string().contains("`web` would hold"),
        "{refusal}"
    );
}
