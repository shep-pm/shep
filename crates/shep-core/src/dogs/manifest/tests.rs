use super::*;

/// A manifest as a dog written against this format prints it. A literal, so
/// a rename of a field fails here instead of passing by construction.
const FIXTURE: &str = r#"{
  "manifest": 1,
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
  "validate": true
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
    manifest
}

#[test]
fn every_v1_field_reads_from_its_documented_name() {
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

    let manifest: Manifest = serde_json::from_value(value).unwrap();

    assert_eq!(manifest, fixture());
}

#[test]
fn a_manifest_that_declares_nothing_reads_with_nothing_declared() {
    let manifest: Manifest = serde_json::from_str(
        r#"{"manifest": 1, "name": "shep-otel", "version": "0.1.3", "protocol": 11}"#,
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
            "manifest": 1,
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
    let text = r#"{"manifest": 1, "name": "shep-sandbox", "version": "0.1.0", "protocol": 11, "launch": true}"#;

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
