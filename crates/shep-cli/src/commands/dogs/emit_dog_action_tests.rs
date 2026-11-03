//! `emit_dog_action`: the one place a dog verb's row reaches stdout.

use shep_core::protocol::DogSource;

use super::emit_dog_action;
use super::test_support::streams;
use crate::cli::Format;
use crate::exit::ExitCode;
use crate::output::DogActionStatus;

#[test]
fn emit_dog_action_wraps_the_row_in_the_verb_s_envelope_under_json() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = emit_dog_action(
        &mut streams,
        "disable",
        "bark",
        DogSource::BuiltIn {
            grants: None,
            events: Vec::new(),
        },
        DogActionStatus::Acted("stopped"),
    );

    assert_eq!(code, ExitCode::Success);
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["command"], "disable");
    assert_eq!(value["data"]["name"], "bark");
    assert_eq!(value["data"]["status"], "stopped");
    assert_eq!(value["data"]["shepherd_acted"], true);
    assert!(err.is_empty());
}

#[test]
fn emit_dog_action_carries_shepherd_acted_false_when_no_shepherd_answered() {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut streams = streams(&mut out, &mut err);
    streams.fmt = Format::Json;

    let code = emit_dog_action(
        &mut streams,
        "enable",
        "bark",
        None,
        DogActionStatus::NoShepherd("will start"),
    );

    assert_eq!(code, ExitCode::Success);
    let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(value["command"], "enable");
    assert_eq!(value["data"]["shepherd_acted"], false);
}

#[test]
fn emit_dog_action_writes_the_table_row_under_table() {
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = emit_dog_action(
        &mut streams(&mut out, &mut err),
        "adopt",
        "otel",
        None,
        DogActionStatus::Acted("running"),
    );

    assert_eq!(code, ExitCode::Success);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("otel"), "{text}");
    assert!(text.contains("running"), "{text}");
}
