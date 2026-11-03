use super::*;

/// A manifest as a dog written against this format prints it. A literal, so
/// a rename of a field fails here instead of passing by construction.
const FIXTURE: &str = r#"{
  "manifest": 2,
  "name": "shep-otel",
  "version": "0.1.3",
  "protocol": 11,
  "channel": true,
  "settings": {"type": "object"},
  "defaults": "interval = \"1h\"\n",
  "permissions": [
    {"name": "flock.read", "required": true, "reason": "lists the flock"}
  ],
  "verb": {
    "description": "Export metrics",
    "subcommands": [{"name": "status", "about": "Show the exporter"}]
  },
  "depends": {
    "dogs": [{"name": "metrics", "version": ">=0.2"}],
    "shep": "0.9.0"
  },
  "validate": true,
  "serves": "every_sheep",
  "actions": [
    {"name": "export-now", "description": "Export without waiting for the interval"}
  ],
  "events": [
    {"kind": "exported", "description": "A batch left for the collector"}
  ]
}"#;

fn fixture() -> Manifest {
    let mut manifest = Manifest::new("shep-otel", "0.1.3", 11);
    manifest.channel = true;
    manifest.settings = Some(serde_json::json!({"type": "object"}));
    manifest.defaults = Some("interval = \"1h\"\n".to_string());
    manifest.permissions = vec![Permission::new("flock.read", true, "lists the flock")];
    let mut verb = Verb::new("Export metrics");
    verb.subcommands = vec![Subcommand::new("status", "Show the exporter")];
    manifest.verb = Some(verb);
    manifest.depends.dogs = vec![DogDependency::new("metrics", ">=0.2")];
    manifest.depends.shep = Some("0.9.0".to_string());
    manifest.validate = true;
    manifest.serves = Serves::EverySheep;
    manifest.actions = vec![Action::new(
        "export-now",
        "Export without waiting for the interval",
    )];
    manifest.events = vec![DogEvent::new("exported", "A batch left for the collector")];
    manifest
}

#[test]
fn every_field_reads_from_its_documented_name() {
    assert_eq!(
        serde_json::from_str::<Manifest>(FIXTURE).unwrap(),
        fixture()
    );
}

#[test]
fn a_manifest_round_trips() {
    let text = serde_json::to_string(&fixture()).unwrap();
    assert_eq!(serde_json::from_str::<Manifest>(&text).unwrap(), fixture());
}

#[test]
fn a_field_this_shep_does_not_know_is_ignored_at_every_level() {
    let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    value["panes"] = serde_json::json!([{"name": "later"}]);
    value["permissions"][0]["scope"] = serde_json::json!("later");
    value["verb"]["aliases"] = serde_json::json!(["later"]);
    value["verb"]["subcommands"][0]["hidden"] = serde_json::json!(true);
    value["depends"]["os"] = serde_json::json!("later");
    value["depends"]["dogs"][0]["optional"] = serde_json::json!(true);
    value["actions"][0]["hidden"] = serde_json::json!(true);
    value["events"][0]["hidden"] = serde_json::json!(true);

    let manifest: Manifest = serde_json::from_value(value).unwrap();

    assert_eq!(manifest, fixture());
}

#[test]
fn a_manifest_that_declares_nothing_reads_with_nothing_declared() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 2, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
    )
    .unwrap();

    assert_eq!(manifest, Manifest::new("shep-otel", "0.1.3", 11));
}

#[test]
fn output_missing_what_identifies_a_dog_is_not_a_manifest() {
    for text in [
        r#"{"name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
        r#"{"manifest": 1, "version": "0.1.3", "protocol": 11}"#,
        r#"{"manifest": 1, "name": "shep-otel", "protocol": 11}"#,
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3"}"#,
        "shep-otel 0.1.3\nshep-protocol: 11\n",
        "",
    ] {
        assert!(serde_json::from_str::<Manifest>(text).is_err(), "{text:?}");
    }
}

#[test]
fn a_manifest_that_declares_nothing_prints_only_who_the_dog_is() {
    let value = serde_json::to_value(Manifest::new("shep-otel", "0.1.3", 11)).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "manifest": 2,
            "name": "shep-otel",
            "version": "0.1.3",
            "protocol": 11,
            "channel": false,
            "validate": false,
            "launch": false,
        })
    );
}

#[test]
fn a_manifest_without_actions_reads_with_none_declared() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
    )
    .unwrap();

    assert!(manifest.actions.is_empty());
}

#[test]
fn declared_actions_read_from_their_documented_names_and_round_trip() {
    let text = r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11,
        "actions": [{"name": "export-now", "description": "Export at once"}, {"name": "pause", "description": "Stop exporting"}]}"#;

    let manifest: Manifest = serde_json::from_str(text).unwrap();

    let expected = vec![
        Action::new("export-now", "Export at once"),
        Action::new("pause", "Stop exporting"),
    ];
    assert_eq!(manifest.actions, expected);
    let again = serde_json::to_string(&manifest).unwrap();
    assert_eq!(
        serde_json::from_str::<Manifest>(&again).unwrap().actions,
        expected
    );
}

#[test]
fn version_zero_is_not_a_manifest_and_a_later_version_still_reads() {
    let with = |version: u32| {
        serde_json::from_value::<Manifest>(serde_json::json!({
            "manifest": version, "name": "shep-otel", "version": "0.1.3", "protocol": 11,
        }))
    };

    assert!(with(0).is_err());
    assert_eq!(with(2).unwrap().manifest, 2);
}

#[test]
fn a_manifest_that_declares_launch_reads_it_and_round_trips() {
    let text = r#"{"manifest": 2, "name": "shep-sandbox", "version": "0.1.0", "protocol": 11, "launch": true}"#;

    let manifest: Manifest = serde_json::from_str(text).unwrap();

    let mut expected = Manifest::new("shep-sandbox", "0.1.0", 11);
    expected.launch = true;
    assert_eq!(manifest, expected);
    let again = serde_json::to_string(&manifest).unwrap();
    assert_eq!(serde_json::from_str::<Manifest>(&again).unwrap(), expected);
}

#[test]
fn a_manifest_without_launch_reads_it_as_false() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
    )
    .unwrap();

    assert!(!manifest.launch);
}

#[test]
fn a_manifest_without_serves_reads_as_opted_in() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
    )
    .unwrap();

    assert_eq!(manifest.serves, Serves::OptedIn);
}

#[test]
fn both_serves_values_read_from_their_documented_names_and_round_trip() {
    for (name, serves) in [
        ("opted_in", Serves::OptedIn),
        ("every_sheep", Serves::EverySheep),
    ] {
        let text = format!(
            r#"{{"manifest": 1, "name": "shep-jobs", "version": "0.1.0", "protocol": 11, "serves": "{name}"}}"#
        );

        let manifest: Manifest = serde_json::from_str(&text).unwrap();

        assert_eq!(manifest.serves, serves, "{name}");
        let again = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            serde_json::from_str::<Manifest>(&again).unwrap().serves,
            serves,
            "{name} after a round trip"
        );
    }
}

#[test]
fn a_dog_serving_every_sheep_prints_it_and_one_opted_in_prints_nothing() {
    let mut every = Manifest::new("shep-jobs", "0.1.0", 11);
    every.serves = Serves::EverySheep;

    assert_eq!(
        serde_json::to_value(&every).unwrap()["serves"],
        serde_json::json!("every_sheep")
    );
    assert_eq!(
        serde_json::to_value(Manifest::new("shep-jobs", "0.1.0", 11))
            .unwrap()
            .get("serves"),
        None
    );
}

#[test]
fn a_v1_manifest_reads_with_no_events_declared() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
    )
    .unwrap();

    assert!(manifest.events.is_empty());
}

#[test]
fn declared_events_read_from_their_documented_names_and_round_trip() {
    let text = r#"{"manifest": 2, "name": "shep-otel", "version": "0.1.3", "protocol": 11,
        "events": [{"kind": "exported", "description": "A batch left"}, {"kind": "failed", "description": "A batch was dropped"}]}"#;

    let manifest: Manifest = serde_json::from_str(text).unwrap();

    let expected = vec![
        DogEvent::new("exported", "A batch left"),
        DogEvent::new("failed", "A batch was dropped"),
    ];
    assert_eq!(manifest.manifest, 2);
    assert_eq!(manifest.events, expected);
    let again = serde_json::to_string(&manifest).unwrap();
    assert_eq!(
        serde_json::from_str::<Manifest>(&again).unwrap().events,
        expected
    );
}

#[test]
fn a_new_manifest_is_version_two() {
    assert_eq!(MANIFEST_VERSION, 2);
    assert_eq!(Manifest::new("shep-otel", "0.1.3", 11).manifest, 2);
}
