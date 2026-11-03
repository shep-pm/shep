//! A declined adopt of a dog that may already be running: the config says
//! disabled, so a shepherd that is running it is told to stop it.

use shep_client::testing::{
    control_address, fake_client_answering, fake_client_capturing_envelopes,
    fake_client_replying_err,
};
use shep_core::protocol::{DogSource, Request, Response, RpcErrorCode};

use super::adopt::declined_after_config;
use super::test_support::streams;
use super::{DECLINED_STATUS, DISABLED_STATUS};
use crate::cli::Format;
use crate::exit::ExitCode;

fn otel() -> DogSource {
    DogSource::Adopted {
        grants: None,
        path: "/usr/local/bin/shep-otel".to_string(),
        channel: false,
        events: Vec::new(),
    }
}

#[tokio::test]
async fn a_no_asks_the_shepherd_to_stop_the_dog_it_may_be_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let mut out = Vec::new();
    let mut err = Vec::new();

    declined_after_config(
        &mut streams(&mut out, &mut err),
        "otel",
        otel(),
        Some(&client),
    )
    .await;

    let sent = envelopes.recv().await.unwrap();
    assert_eq!(
        sent.body,
        Request::DisableDog {
            name: "otel".to_string()
        }
    );
}

#[tokio::test]
async fn a_no_reports_the_dog_stopped_when_a_shepherd_answered() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, |_| Response::Deleted(vec![7])).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = declined_after_config(&mut streams, "otel", otel(), Some(&client)).await;

    assert_eq!(code, ExitCode::Success);
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["data"]["shepherd_acted"], serde_json::json!(true));
    assert_eq!(json["data"]["status"], DISABLED_STATUS.text());
}

#[tokio::test]
async fn a_no_with_no_shepherd_reports_nothing_was_granted() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = declined_after_config(&mut streams, "otel", otel(), None).await;

    assert_eq!(code, ExitCode::Success);
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["data"]["shepherd_acted"], serde_json::json!(false));
    assert_eq!(json["data"]["status"], DECLINED_STATUS.text());
}

/// A first-time adopt the shepherd has never registered: it answers
/// `NotFound` to the stop, which is nothing running, not a failure.
#[tokio::test]
async fn a_no_for_a_dog_the_shepherd_never_registered_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = control_address(dir.path());
    let (client, _daemon) =
        fake_client_replying_err(&path, RpcErrorCode::NotFound, "no such dog").await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = declined_after_config(&mut streams, "otel", otel(), Some(&client)).await;

    assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(json["data"]["status"], DECLINED_STATUS.text());
}
