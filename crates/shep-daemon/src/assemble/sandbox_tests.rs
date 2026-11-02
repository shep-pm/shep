use super::assemble;
#[cfg(unix)]
use super::describe;
use super::testing::*;
use shep_core::config::{AppConfig, DogTable, normalize};

fn sandboxed(edit: impl FnOnce(&mut AppConfig)) -> crate::runner::SpawnSpec {
    let mut config = AppConfig::minimal("app", "/opt/bin/server");
    config.args = vec!["--port".to_string(), "8080".to_string()];
    edit(&mut config);
    let app = normalize(config).unwrap();
    assemble(&app, 0, &test_paths(), None, &no_secrets()).unwrap()
}

#[test]
fn no_sandbox_leaves_the_command_and_the_stop_signal_alone() {
    let spec = sandboxed(|_| {});

    assert_eq!(spec.program, "/opt/bin/server");
    assert_eq!(spec.args, vec!["--port", "8080"]);
    assert!(!spec.leader_only_stop);
}

fn table(value: serde_json::Value) -> DogTable {
    serde_json::from_value(value).unwrap()
}

#[test]
#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
fn a_sheep_with_a_sandbox_dog_table_stops_by_its_leader_alone() {
    let spec = sandboxed(|c| {
        c.dogs
            .insert("sandbox".to_string(), table(serde_json::json!({})));
    });

    assert!(spec.leader_only_stop);
    assert_eq!(spec.program, "/opt/bin/server");
}

#[test]
fn a_sheep_with_a_table_for_another_dog_stops_by_its_group() {
    let spec = sandboxed(|c| {
        c.dogs.insert(
            "jobs".to_string(),
            table(serde_json::json!({ "concurrency": 2 })),
        );
    });

    assert!(!spec.leader_only_stop);
}

#[test]
#[cfg(unix)] // normalize refuses a sandbox dog table on Windows.
fn describe_and_assemble_agree_on_the_leader_only_stop_for_a_sandbox_table() {
    let mut config = AppConfig::minimal("app", "/opt/bin/server");
    config
        .dogs
        .insert("sandbox".to_string(), table(serde_json::json!({})));
    let app = normalize(config).unwrap();
    let paths = test_paths();

    let assembled = assemble(&app, 0, &paths, None, &no_secrets()).unwrap();
    let described = describe(&app, 0, &paths, None, &no_secrets());

    assert!(assembled.leader_only_stop);
    assert_eq!(assembled.leader_only_stop, described.leader_only_stop);
}
