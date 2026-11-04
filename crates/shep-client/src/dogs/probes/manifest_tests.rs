//! A fixture dog's `Probe`, asked each of the three flags.

use shep_core::dogs::manifest::{
    Action, DogDependency, DogEvent, Manifest, Permission, Serves, Subcommand, Verb,
};
use shep_core::dogs::{MANIFEST_FLAG, SCHEMA_FLAG, VERSION_FLAG};

use super::*;
use crate::dogs::dog_config;

#[dog_config]
#[derive(schemars::JsonSchema)]
#[allow(dead_code, reason = "read by the generated schema, not by Rust")]
struct Fixture {
    interval: String,
}

fn fixture_schema() -> Option<serde_json::Value> {
    Some(config_schema::<Fixture>().as_value().clone())
}

/// A dog that declares something under every builder call.
fn fixture() -> Probe<'static> {
    Probe::new("shep-otel", "0.1.3")
        .ask_for_channel()
        .permission(Permission::new("flock.read", true, "lists the flock"))
        .permission(Permission::new("logs.read", false, "tails logs"))
        .verb({
            let mut verb = Verb::new("Export metrics").with_examples(["shep otel status"]);
            verb.subcommands =
                vec![Subcommand::new("status", "Show the exporter").with_usage("<job> [watch]")];
            verb
        })
        .depends_on_dog(DogDependency::new("metrics", ">=0.2"))
        .depends_on_shep("0.9.0")
        .defaults("interval = \"1h\"\n")
        .validate()
        .serves_every_sheep()
        .action("export-now", "Export without waiting for the interval")
        .event("exported", "A batch was sent")
}

fn reply(probe: &Probe<'_>, flag: &str) -> String {
    probe
        .reply(Some(flag), fixture_schema)
        .unwrap_or_else(|| panic!("{flag} was not answered"))
}

#[test]
fn the_manifest_parses_back_to_what_the_dog_declared() {
    let text = reply(&fixture(), MANIFEST_FLAG);
    let manifest: Manifest = serde_json::from_str(&text).unwrap();

    let mut expected = Manifest::new("shep-otel", "0.1.3", crate::PROTOCOL_VERSION);
    expected.channel = true;
    expected.settings = fixture_schema();
    expected.defaults = Some("interval = \"1h\"\n".to_string());
    expected.permissions = vec![
        Permission::new("flock.read", true, "lists the flock"),
        Permission::new("logs.read", false, "tails logs"),
    ];
    let mut verb = Verb::new("Export metrics").with_examples(["shep otel status"]);
    verb.subcommands =
        vec![Subcommand::new("status", "Show the exporter").with_usage("<job> [watch]")];
    expected.verb = Some(verb);
    expected.depends.dogs = vec![DogDependency::new("metrics", ">=0.2")];
    expected.depends.shep = Some("0.9.0".to_string());
    expected.validate = true;
    expected.serves = Serves::EverySheep;
    expected.actions = vec![Action::new(
        "export-now",
        "Export without waiting for the interval",
    )];
    expected.events = vec![DogEvent::new("exported", "A batch was sent")];
    assert_eq!(manifest, expected);
}

#[test]
fn serves_can_be_set_to_either_value() {
    for serves in [Serves::OptedIn, Serves::EverySheep] {
        let text = reply(
            &Probe::new("shep-jobs", "0.1.0").serves(serves),
            MANIFEST_FLAG,
        );
        let manifest: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(manifest.serves, serves);
    }
}

#[test]
fn the_manifest_settings_are_the_schema_flag_answer_byte_for_byte() {
    let probe = fixture();
    let manifest: Manifest = serde_json::from_str(&reply(&probe, MANIFEST_FLAG)).unwrap();

    let settings = serde_json::to_string_pretty(&manifest.settings.unwrap()).unwrap();
    assert_eq!(format!("{settings}\n"), reply(&probe, SCHEMA_FLAG));
}

#[test]
fn the_version_and_schema_flags_answer_as_they_did_before_the_manifest() {
    let probe = fixture();

    assert_eq!(
        reply(&probe, VERSION_FLAG),
        format!(
            "shep-otel 0.1.3\nshep-protocol: {}\nshep-channel: true\n",
            crate::PROTOCOL_VERSION
        )
    );
    let schema: serde_json::Value = serde_json::from_str(&reply(&probe, SCHEMA_FLAG)).unwrap();
    assert_eq!(Some(schema), fixture_schema());
}

#[test]
fn a_dog_that_declares_nothing_prints_who_it_is_and_its_settings() {
    let text = reply(&Probe::new("shep-otel", "0.1.3"), MANIFEST_FLAG);
    let manifest: Manifest = serde_json::from_str(&text).unwrap();

    let mut expected = Manifest::new("shep-otel", "0.1.3", crate::PROTOCOL_VERSION);
    expected.settings = fixture_schema();
    assert_eq!(manifest, expected);
}

#[test]
fn a_run_that_is_not_a_probe_answers_nothing() {
    let probe = fixture();

    assert_eq!(probe.reply(None, fixture_schema), None);
    assert_eq!(probe.reply(Some("run"), fixture_schema), None);
}

#[test]
fn a_dog_with_no_schema_prints_a_manifest_without_settings_and_an_empty_schema_answer() {
    let probe = fixture();

    let text = probe.reply(Some(MANIFEST_FLAG), || None).unwrap();
    let manifest: Manifest = serde_json::from_str(&text).unwrap();
    assert_eq!(manifest.settings, None);
    assert_eq!(probe.reply(Some(SCHEMA_FLAG), || None).unwrap(), "");
}

#[test]
fn a_probe_that_declares_launch_prints_it_in_the_manifest() {
    let text = reply(&Probe::new("shep-sandbox", "0.1.0").launch(), MANIFEST_FLAG);
    let manifest: Manifest = serde_json::from_str(&text).unwrap();

    let mut expected = Manifest::new("shep-sandbox", "0.1.0", crate::PROTOCOL_VERSION);
    expected.settings = fixture_schema();
    expected.launch = true;
    assert_eq!(manifest, expected);
}

#[test]
fn a_probe_that_declares_a_pane_with_the_channel_prints_it_in_the_manifest() {
    let probe = Probe::new("shep-gpu", "0.1.0").ask_for_channel().pane();
    let manifest: Manifest = serde_json::from_str(&reply(&probe, MANIFEST_FLAG)).unwrap();

    assert!(manifest.pane);
    assert!(!manifest.pane_ignored);
}

/// `shep adopt` reads a pane without a channel as no pane, so the manifest a
/// dog that forgot the channel prints is read as having none.
#[test]
fn a_probe_that_declares_no_pane_prints_none() {
    let probe = Probe::new("shep-gpu", "0.1.0").ask_for_channel();
    let manifest: Manifest = serde_json::from_str(&reply(&probe, MANIFEST_FLAG)).unwrap();

    assert!(!manifest.pane);
    assert!(!manifest.pane_ignored);
}

#[test]
fn a_pane_without_the_channel_is_read_as_no_pane() {
    let probe = Probe::new("shep-gpu", "0.1.0").pane();
    let manifest: Manifest = serde_json::from_str(&reply(&probe, MANIFEST_FLAG)).unwrap();

    assert!(!manifest.pane);
    assert!(manifest.pane_ignored);
}
