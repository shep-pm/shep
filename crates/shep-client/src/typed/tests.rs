use std::collections::BTreeMap;
use std::time::Duration;

use shep_core::protocol::{DogSource, Envelope, HostUsage, RpcError, RpcErrorCode};
use shep_core::status::ProcStatus;
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::testing::{
    Handovers, Handshake, control_address, fake_client_answering, fake_client_answering_refusals,
    fake_daemon_across_handovers, sample_ack, sample_info, serve_one_request,
};

/// The sentinel stands for a secret's value: it must reach no error text.
const SENTINEL: &str = "sentinel-value-1f3a";

/// What a shepherd that has never heard of the request answers.
fn does_not_know_the_request() -> RpcError {
    RpcError {
        code: RpcErrorCode::Unsupported,
        message: "this shepherd does not implement the request".to_owned(),
        daemon_version: None,
    }
}

/// A shepherd built before `DogConfigResolved`, whose section is `section`.
fn older_shepherd(path: &std::path::Path, section: &str) -> Handovers {
    let shepherd = fake_daemon_across_handovers(path, vec![Handshake::Accept(sample_ack())]);
    shepherd.reply_to_dog_config(section);
    shepherd.refuse_dog_config_resolved(does_not_know_the_request());
    shepherd
}

/// The request bodies the shepherd received, in order.
fn asked(shepherd: &Handovers) -> Vec<Request> {
    shepherd
        .envelopes()
        .into_iter()
        .map(|(_, envelope)| envelope.body)
        .collect()
}

fn named(name: &str) -> (Request, Request) {
    (
        Request::DogConfigResolved {
            name: name.to_owned(),
        },
        Request::DogConfig {
            name: name.to_owned(),
        },
    )
}

fn too_old_refusal(name: &str) -> RequestError {
    RequestError::Rpc(RpcError {
        code: RpcErrorCode::Unsupported,
        message: format!(
            "the shepherd is too old to resolve secret references in the `{name}` dog's section"
        ),
        daemon_version: None,
    })
}

/// The one envelope the client under test sent.
async fn sent(envelopes: &mut UnboundedReceiver<Envelope>) -> Envelope {
    tokio::time::timeout(Duration::from_secs(5), envelopes.recv())
        .await
        .expect("the request reached the fake daemon")
        .expect("the fake daemon is still up")
}

#[tokio::test]
async fn publish_event_sends_the_kind_and_the_payload_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::EventPublished).await;
    let payload = serde_json::json!({ "batch": 3, "rows": [1, null, "x"] });

    client
        .publish_event("exported", payload.clone())
        .await
        .unwrap();

    assert_eq!(
        sent(&mut envelopes).await.body,
        Request::PublishEvent {
            event: "exported".to_owned(),
            payload,
        }
    );
}

#[tokio::test]
async fn publish_event_refused_by_the_shepherd_is_the_rpc_error() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = fake_client_answering_refusals(&control_address(dir.path()), |_| {
        Err(RpcError {
            code: RpcErrorCode::Forbidden,
            message: "dog `bark` did not declare the event kind `dropped`".to_owned(),
            daemon_version: None,
        })
    })
    .await;

    let err = client
        .publish_event("dropped", serde_json::Value::Null)
        .await
        .unwrap_err();

    assert!(
        matches!(err, RequestError::Rpc(ref rpc) if rpc.code == RpcErrorCode::Forbidden),
        "{err}"
    );
}

#[tokio::test]
async fn publish_event_answered_with_anything_else_is_an_unexpected_reply() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::Pong).await;

    let err = client
        .publish_event("exported", serde_json::Value::Null)
        .await
        .unwrap_err();

    assert!(
        matches!(
            err,
            RequestError::UnexpectedReply {
                asked: "PublishEvent",
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn dog_config_asks_by_name_and_answers_the_section() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::DogSection {
            toml: "poll = \"1m\"\n".to_owned().into(),
        })
        .await;

    let section = client.dog_config("bark").await.unwrap();

    assert_eq!(section.as_str(), "poll = \"1m\"\n");
    assert_eq!(
        sent(&mut envelopes).await.body,
        Request::DogConfigResolved {
            name: "bark".to_owned()
        }
    );
}

#[tokio::test]
async fn list_flock_answers_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), |_| {
        Response::Flock(vec![sample_info()])
    })
    .await;

    assert_eq!(client.list_flock().await.unwrap(), vec![sample_info()]);
}

/// The log plane walks the flock file by file, so the default budget
/// can run out on a slow disk.
#[tokio::test]
async fn reopen_names_the_selector_and_asks_for_the_log_plane_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = fake_client_answering(&control_address(dir.path()), |_| {
        Response::Reopened(vec![sample_info()])
    })
    .await;

    let reopened = client
        .reopen(SelectorSpec::Name("web".to_owned()))
        .await
        .unwrap();

    assert_eq!(reopened, vec![sample_info()]);
    let envelope = sent(&mut envelopes).await;
    assert_eq!(
        envelope.body,
        Request::Reopen {
            selector: SelectorSpec::Name("web".to_owned())
        }
    );
    let budget = u64::try_from(LOG_PLANE_DEADLINE.as_millis()).unwrap();
    assert_eq!(envelope.deadline_ms, Some(budget));
}

#[tokio::test]
async fn host_usage_answers_the_reading() {
    let dir = tempfile::tempdir().unwrap();
    let reading = HostUsage {
        cpu_percent: None,
        memory_used_bytes: 1,
        memory_total_bytes: 2,
        disk_bytes_per_second: None,
        network_bytes_per_second: None,
    };
    let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), move |_| {
        Response::HostUsage(Some(reading))
    })
    .await;

    assert_eq!(client.host_usage().await.unwrap(), Some(reading));
}

fn two_lines() -> LogTail {
    LogTail::Lines {
        lines: vec!["listening".to_owned(), "ready".to_owned()],
        truncated: true,
    }
}

#[tokio::test]
async fn tail_log_sends_the_request_and_returns_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) = fake_client_answering(&control_address(dir.path()), |_| {
        Response::LogTail(two_lines())
    })
    .await;

    let tail = client.tail_log(3, LogStream::Err, 20).await.unwrap();

    assert_eq!(tail, two_lines());
    let envelope = sent(&mut envelopes).await;
    assert_eq!(
        envelope.body,
        Request::TailLog {
            id: 3,
            stream: LogStream::Err,
            lines: 20
        }
    );
    let budget = u64::try_from(crate::client::DEFAULT_DEADLINE.as_millis()).unwrap();
    assert_eq!(envelope.deadline_ms, Some(budget), "the default deadline");
}

#[tokio::test]
async fn tail_log_refuses_any_other_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::Pong).await;

    let err = client.tail_log(3, LogStream::Out, 20).await.unwrap_err();

    assert_eq!(
        err,
        RequestError::UnexpectedReply {
            asked: "TailLog",
            answered: "Pong",
        }
    );
}

#[tokio::test]
async fn a_reconnecting_client_tail_log_sends_the_request_and_returns_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let socket = control_address(dir.path());
    let served = serve_one_request(&socket, sample_ack(), Response::LogTail(two_lines())).await;
    let client = ReconnectingClient::connect(&socket).await.unwrap();

    let tail = client.tail_log(3, LogStream::Out, 5).await.unwrap();

    assert_eq!(tail, two_lines());
    let envelope = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the request reached the fake daemon")
        .unwrap();
    assert_eq!(
        envelope.body,
        Request::TailLog {
            id: 3,
            stream: LogStream::Out,
            lines: 5
        }
    );
}

#[tokio::test]
async fn a_reconnecting_client_tail_log_refuses_any_other_answer() {
    let dir = tempfile::tempdir().unwrap();
    let socket = control_address(dir.path());
    let _served = serve_one_request(&socket, sample_ack(), Response::Pong).await;
    let client = ReconnectingClient::connect(&socket).await.unwrap();

    let err = client.tail_log(3, LogStream::Out, 5).await.unwrap_err();

    assert_eq!(
        err,
        RequestError::UnexpectedReply {
            asked: "TailLog",
            answered: "Pong",
        }
    );
}

/// The error names both variants and never the section's body.
#[tokio::test]
async fn another_answer_is_named_by_variant_alone() {
    let dir = tempfile::tempdir().unwrap();
    let secret = "https://example.invalid/hook/super-secret-token";
    let (client, _envelopes) = fake_client_answering(&control_address(dir.path()), move |_| {
        Response::DogSection {
            toml: format!("webhook = \"{secret}\"\n").into(),
        }
    })
    .await;

    let err = client.list_flock().await.unwrap_err();

    assert_eq!(
        err,
        RequestError::UnexpectedReply {
            asked: "ListFlock",
            answered: "DogSection",
        }
    );
    assert_eq!(
        err.to_string(),
        "the daemon answered ListFlock with DogSection"
    );
}

#[tokio::test]
async fn a_reconnecting_client_asks_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let socket = control_address(dir.path());
    let served = serve_one_request(
        &socket,
        sample_ack(),
        Response::DogSection {
            toml: String::new().into(),
        },
    )
    .await;
    let client = ReconnectingClient::connect(&socket).await.unwrap();

    let section = client.dog_config("metrics").await.unwrap();

    assert_eq!(section.as_str(), "");
    let envelope = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the request reached the fake daemon")
        .unwrap();
    assert_eq!(
        envelope.body,
        Request::DogConfigResolved {
            name: "metrics".to_owned()
        }
    );
}

#[tokio::test]
async fn an_older_shepherd_that_does_not_know_the_request_is_asked_the_plain_one_when_the_section_names_no_reference()
 {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let shepherd = older_shepherd(&path, "poll = \"1m\"\n");
    let client = Client::connect(&path).await.unwrap();

    let section = client.dog_config("bark").await.unwrap();

    assert_eq!(section.as_str(), "poll = \"1m\"\n");
    let (resolved, plain) = named("bark");
    assert_eq!(asked(&shepherd), vec![resolved, plain]);
}

#[tokio::test]
async fn an_older_shepherd_with_a_reference_in_the_section_is_refused_without_the_section_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let _shepherd = older_shepherd(&path, &format!("token = \"{{{{secret:{SENTINEL}}}}}\"\n"));
    let client = Client::connect(&path).await.unwrap();

    let err = client.dog_config("bark").await.unwrap_err();

    assert_eq!(err, too_old_refusal("bark"));
    let text = format!("{err} {err:?}");
    assert!(!text.contains(SENTINEL), "{text}");
    assert!(!text.contains("{{secret:"), "{text}");
}

/// An escaped `{{{{secret:` and a malformed token name no reference, so
/// the plain section is safe to hand over.
#[tokio::test]
async fn an_older_shepherd_with_only_text_that_looks_like_a_reference_is_read_as_before() {
    for text in [
        "note = \"{{{{secret:X}}}}\"\n",
        "note = \"{{secret:bad key}}\"\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = control_address(dir.path());
        let _shepherd = older_shepherd(&path, text);
        let client = Client::connect(&path).await.unwrap();

        let section = client.dog_config("bark").await.unwrap();

        assert_eq!(section.as_str(), text);
    }
}

#[tokio::test]
async fn a_reconnecting_client_on_an_older_shepherd_asks_the_plain_one_when_the_section_names_no_reference()
 {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let shepherd = older_shepherd(&path, "poll = \"1m\"\n");
    let client = ReconnectingClient::connect(&path).await.unwrap();

    let section = client.dog_config("metrics").await.unwrap();

    assert_eq!(section.as_str(), "poll = \"1m\"\n");
    let (resolved, plain) = named("metrics");
    assert_eq!(asked(&shepherd), vec![resolved, plain]);
}

#[tokio::test]
async fn a_reconnecting_client_on_an_older_shepherd_with_a_reference_is_refused_without_the_section_text()
 {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let _shepherd = older_shepherd(&path, &format!("token = \"{{{{secret:{SENTINEL}}}}}\"\n"));
    let client = ReconnectingClient::connect(&path).await.unwrap();

    let err = client.dog_config("metrics").await.unwrap_err();

    assert_eq!(err, too_old_refusal("metrics"));
    let text = format!("{err} {err:?}");
    assert!(!text.contains(SENTINEL), "{text}");
    assert!(!text.contains("{{secret:"), "{text}");
}

fn deploy_table(token: &str) -> DogTable {
    DogTable::from(serde_json::Map::from_iter([(
        "env".to_owned(),
        serde_json::json!({ "TOKEN": token }),
    )]))
}

/// A shepherd built before `SheepDogSettingsResolved`, whose `deploy`
/// table on `web` is `table`.
fn older_shepherd_with_table(path: &std::path::Path, table: DogTable) -> Handovers {
    let shepherd = fake_daemon_across_handovers(path, vec![Handshake::Accept(sample_ack())]);
    shepherd.reply_to_dog_sheep_settings(BTreeMap::from([("web".to_owned(), table)]));
    shepherd.reply_to_list(vec![
        sheep_named(1, "web"),
        sheep_named(2, "api"),
        // A dog's own name is not a sheep's.
        ProcessInfo::builder(3, "bark", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn {
                grants: None,
                events: Vec::new(),
            }))
            .build(),
    ]);
    shepherd.refuse_dog_config_resolved(does_not_know_the_request());
    shepherd
}

fn sheep_named(id: u32, name: &str) -> ProcessInfo {
    ProcessInfo::builder(id, name, ProcStatus::Online).build()
}

fn resolved_and_plain_table_reads() -> Vec<Request> {
    vec![
        Request::SheepDogSettingsResolved {
            name: "web".to_owned(),
            dog: "deploy".to_owned(),
        },
        Request::DogSheepSettings {
            dog: "deploy".to_owned(),
        },
    ]
}

#[tokio::test]
async fn sheep_dog_table_asks_by_name_and_dog() {
    let dir = tempfile::tempdir().unwrap();
    let (client, mut envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::SheepDogTable {
            table: Some(deploy_table("resolved")),
        })
        .await;

    let table = client.sheep_dog_table("web", "deploy").await.unwrap();

    assert_eq!(table, Some(deploy_table("resolved")));
    assert_eq!(
        sent(&mut envelopes).await.body,
        Request::SheepDogSettingsResolved {
            name: "web".to_owned(),
            dog: "deploy".to_owned(),
        }
    );
}

#[tokio::test]
async fn a_reconnecting_client_sheep_dog_table_asks_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let socket = control_address(dir.path());
    let served = serve_one_request(
        &socket,
        sample_ack(),
        Response::SheepDogTable { table: None },
    )
    .await;
    let client = ReconnectingClient::connect(&socket).await.unwrap();

    assert_eq!(client.sheep_dog_table("web", "deploy").await.unwrap(), None);

    let envelope = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the request reached the fake daemon")
        .unwrap();
    assert_eq!(envelope.body, resolved_and_plain_table_reads()[0]);
}

#[tokio::test]
async fn sheep_dog_table_refuses_any_other_answer() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _envelopes) =
        fake_client_answering(&control_address(dir.path()), |_| Response::Pong).await;

    let err = client.sheep_dog_table("web", "deploy").await.unwrap_err();

    assert_eq!(
        err,
        RequestError::UnexpectedReply {
            asked: "SheepDogSettingsResolved",
            answered: "Pong",
        }
    );
}

#[tokio::test]
async fn sheep_dog_table_falls_back_to_dog_sheep_settings_on_an_older_shepherd() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let plain = DogTable::from(serde_json::Map::from_iter([(
        "retries".to_owned(),
        serde_json::json!(3),
    )]));
    let shepherd = older_shepherd_with_table(&path, plain.clone());
    let client = Client::connect(&path).await.unwrap();

    assert_eq!(
        client.sheep_dog_table("web", "deploy").await.unwrap(),
        Some(plain)
    );
    assert_eq!(
        client.sheep_dog_table("api", "deploy").await.unwrap(),
        None,
        "a sheep the older shepherd lists no table for"
    );
    assert_eq!(asked(&shepherd)[..2], resolved_and_plain_table_reads());
}

#[tokio::test]
async fn sheep_dog_table_on_an_older_shepherd_is_not_found_for_a_name_no_sheep_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let _shepherd = older_shepherd_with_table(&path, deploy_table("plain"));
    let client = Client::connect(&path).await.unwrap();

    for name in ["ghost", "bark"] {
        let err = client.sheep_dog_table(name, "deploy").await.unwrap_err();

        assert_eq!(
            err,
            RequestError::Rpc(RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("no sheep named {name}"),
                daemon_version: None,
            })
        );
    }
}

#[tokio::test]
async fn sheep_dog_table_refuses_an_older_shepherds_table_holding_a_reference() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let _shepherd =
        older_shepherd_with_table(&path, deploy_table(&format!("{{{{secret:{SENTINEL}}}}}")));
    let client = Client::connect(&path).await.unwrap();

    let err = client.sheep_dog_table("web", "deploy").await.unwrap_err();

    assert_eq!(
        err,
        RequestError::Rpc(RpcError {
            code: RpcErrorCode::Unsupported,
            message: "the shepherd is too old to resolve secret references in the `web` \
                      sheep's `deploy` table"
                .to_owned(),
            daemon_version: None,
        })
    );
    let text = format!("{err} {err:?}");
    assert!(!text.contains(SENTINEL), "{text}");
    assert!(!text.contains("{{secret:"), "{text}");
}
