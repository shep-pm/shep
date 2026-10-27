//! The config pane's own readers: fields, values, cost, locks and reload.

use super::fixtures::{bark_pane, web, web_with_args};
use shep_core::protocol::SheepConfigView;
use shep_core::values::{MemSize, UpDuration};

use shep_core::config::{AppConfig, ApplyGroup, ProbeConfig, ProbeKind};

use super::config::{ConfiguredReadiness, reload_mode};
use super::*;

#[test]
fn a_sheep_pane_has_forty_four_fields_in_nine_groups() {
    let pane = ConfigPane::sheep(web());
    assert_eq!(pane.fields().len(), 44);
    let mut groups: Vec<&str> = Vec::new();
    for field in pane.fields().fields() {
        let group = field.group.as_deref().expect("every field carries a group");
        if groups.last() != Some(&group) {
            groups.push(group);
        }
    }
    assert_eq!(
        groups,
        [
            "process",
            "logging",
            "inputs",
            "restart",
            "readiness",
            "shutdown",
            "watch",
            "cron",
            "dogs"
        ]
    );
}

#[test]
fn a_value_renders_bare_for_a_scalar_and_as_a_count_for_env() {
    let pane = ConfigPane::sheep(web());
    assert_eq!(pane.value("max_restarts"), "32");
    assert_eq!(pane.value("autorestart"), "true");
    assert_eq!(pane.value("cwd"), "(unset)");
    assert_eq!(pane.value("env"), "1 key");
}

#[test]
fn cost_comes_from_apply_group_for_a_sheep() {
    let pane = ConfigPane::sheep(web());
    assert_eq!(pane.cost("max_restarts"), Some(ApplyGroup::Live));
    assert_eq!(pane.cost("kill_signal"), Some(ApplyGroup::NextSpawn));
    assert_eq!(pane.cost("script"), Some(ApplyGroup::NeedsRespawn));
    assert_eq!(pane.cost("instances"), Some(ApplyGroup::Structural));
}

#[test]
fn structural_fields_are_not_editable_and_the_rest_are() {
    let pane = ConfigPane::sheep(web());
    for key in ["name", "instances"] {
        assert!(!pane.fields().by_key(key).unwrap().editable, "{key}");
    }
    assert!(pane.fields().by_key("max_restarts").unwrap().editable);
}

#[test]
fn the_menu_counts_the_parked_fields_once() {
    let mut config = AppConfig {
        name: "web".into(),
        ..AppConfig::default()
    };
    config.env.insert("DB_HOST".into(), "db.internal".into());
    let view = SheepConfigView::new(config, Vec::new(), vec!["env".into(), "kill_signal".into()]);
    assert_eq!(ConfigPane::sheep(view).parked_count(), 2);
    assert_eq!(ConfigPane::sheep(web_with_args(&[])).parked_count(), 0);
}

#[test]
fn a_probe_without_reuse_port_is_the_only_serial_reload() {
    assert_eq!(
        reload_mode(ConfiguredReadiness::Probe, false),
        ReloadKind::Serial,
        "the outgoing instance can answer the probe the replacement must pass"
    );
    assert_eq!(
        reload_mode(ConfiguredReadiness::Channel, false),
        ReloadKind::Overlap,
        "a channel belongs to the instance that opened it"
    );
    assert_eq!(
        reload_mode(ConfiguredReadiness::Probe, true),
        ReloadKind::Overlap,
        "reuse_port means no EADDRINUSE to avoid"
    );
    assert_eq!(
        reload_mode(ConfiguredReadiness::Heuristic, false),
        ReloadKind::Overlap,
        "nothing is being waited on"
    );
}

#[test]
fn a_pane_reads_its_reload_kind_off_the_three_fields_it_turns_on() {
    let probed = |wait_ready, reuse_port| {
        let config = AppConfig {
            name: "web".into(),
            wait_ready,
            reuse_port,
            readiness_probe: Some(ProbeConfig {
                kind: ProbeKind::Tcp,
                target: "127.0.0.1:8080".into(),
                interval: UpDuration::from_millis(10_000),
                timeout: UpDuration::from_millis(5_000),
                failure_threshold: 3,
            }),
            ..AppConfig::default()
        };
        ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new())).reload_kind()
    };
    assert_eq!(probed(false, false), ReloadKind::Serial);
    assert_eq!(probed(true, false), ReloadKind::Overlap);
    assert_eq!(probed(false, true), ReloadKind::Overlap);
    assert_eq!(
        ConfigPane::sheep(web()).reload_kind(),
        ReloadKind::Overlap,
        "no probe, so nothing an outgoing instance could answer"
    );
}

#[test]
fn overridden_and_pending_are_read_off_the_view() {
    let pane = ConfigPane::sheep(web());
    assert!(pane.is_overridden("max_restarts"));
    assert!(!pane.is_overridden("autorestart"));
    assert!(pane.is_pending("env"));
    assert!(!pane.is_pending("max_restarts"));
}

#[test]
fn a_bare_up_duration_shows_its_resolved_unit_and_a_suffixed_one_is_unchanged() {
    let pane = ConfigPane::sheep(web());
    assert_eq!(pane.display_value("kill_timeout"), "1600ms");
    assert_eq!(pane.value("kill_timeout"), "1600");
    assert_eq!(pane.display_value("listen_timeout"), "3s");
}

/// A dog's values come from the section its operator wrote, so a
/// spelling that `UpDuration::Display` would canonicalize to `1m`
/// stays as `60s`. A sheep's arrive already canonicalized, `UpDuration`
/// serializing through its own `Display`, so this is the pane that can
/// tell the difference.
#[test]
fn a_dogs_own_spelling_of_a_duration_survives_the_pane() {
    let schema = serde_json::json!({
        "properties": {
            "poll": { "type": "string", "$ref": "#/$defs/UpDuration" }
        },
        "$defs": { "UpDuration": { "type": "string" } }
    });
    let pane = ConfigPane::dog("bark".into(), None, schema, "poll = \"60s\"\n".into());
    assert_eq!(pane.display_value("poll"), "60s");
}

/// 64 bytes is not a whole `K`/`M`/`G`, so `MemSize::Display` prints
/// the bare digits `"64"`; 512 mebibytes already prints with a unit.
#[test]
fn a_bare_mem_size_shows_its_resolved_unit_and_a_suffixed_one_is_unchanged() {
    for (bytes, want) in [(64, "64 B"), (512 << 20, "512M")] {
        let config = AppConfig {
            name: "web".into(),
            max_memory: Some(MemSize::from_bytes(bytes)),
            ..AppConfig::default()
        };
        let pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
        assert_eq!(pane.display_value("max_memory"), want, "{bytes} bytes");
    }
}

/// A field with no `MemSize`/`UpDuration` grammar is untouched by
/// `display_value`, the same as `value`.
#[test]
fn a_field_with_no_unit_grammar_displays_the_same_either_way() {
    let pane = ConfigPane::sheep(web());
    assert_eq!(pane.display_value("cwd"), pane.value("cwd"));
    assert_eq!(
        pane.display_value("max_restarts"),
        pane.value("max_restarts")
    );
}

/// A dog's schema carries no group, so the pane draws no headers, and
/// shep does not classify a dog's field cost: `cost` answers `None`
/// for one.
#[test]
fn a_dog_pane_is_flat_in_schema_order_and_marks_the_secret() {
    let pane = bark_pane();
    assert!(
        pane.fields()
            .fields()
            .iter()
            .all(|field| field.group.is_none()),
        "a dog's schema carries no group, so the pane draws no headers"
    );
    let keys: Vec<&str> = pane
        .fields()
        .fields()
        .iter()
        .map(|f| f.key.as_str())
        .collect();
    assert_eq!(
        keys,
        ["history_bytes", "poll", "rules", "sink_timeout", "sinks"],
        "schema order, which for a serde_json map is alphabetical"
    );
    assert!(
        pane.fields()
            .by_key("sinks")
            .expect("sinks is a field")
            .secret
    );
    assert_eq!(pane.value("poll"), "60s");
    assert_eq!(pane.cost("poll"), None);
}

/// The `dogs` tab walks dogs, so the sheep's `dogs` field has no row to
/// refuse an edit on.
#[test]
fn the_dogs_tab_draws_no_field_row() {
    let mut pane = ConfigPane::sheep(web());
    pane.set_group(9);
    assert_eq!(pane.group(), "dogs");
    assert!(pane.rows().is_empty(), "{:?}", pane.rows());
    assert_eq!(pane.lock("args"), None);
}

/// Shep writes a `sinks` table happily; this screen simply has no
/// widget for a table of tables.
#[test]
fn a_dogs_map_field_is_locked_for_want_of_a_widget_and_not_refused() {
    let pane = bark_pane();
    assert_eq!(pane.lock("sinks"), Some(Lock::NoWidget));
    assert_eq!(pane.lock("rules"), Some(Lock::NoWidget));
    assert_eq!(pane.lock("poll"), None);
}

/// A dog's list is edited on the same sub-screen a sheep's `args` is, so
/// its row is not locked and the sub-screen opens on the elements the
/// section holds.
#[test]
fn a_dogs_string_list_opens_the_sub_screen_on_its_stored_elements() {
    let schema = serde_json::json!({
        "properties": {
            "hosts": { "type": "array", "items": { "type": "string" } },
        },
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "hosts = [\"a\", \"b\"]\n".to_owned(),
    );
    assert!(pane.fields().by_key("hosts").expect("a field").editable);
    assert_eq!(pane.lock("hosts"), None);
    pane.open_list();
    let list = pane.list().expect("the sub-screen opens");
    assert_eq!(list.key(), "hosts");
    assert_eq!(list.elements(), ["a", "b"]);
}

/// A map has no widget, and a secret list would draw its elements
/// unmasked on the sub-screen, so both stay locked in a dog pane.
#[test]
fn a_dogs_secret_list_and_map_stay_locked() {
    let schema = serde_json::json!({
        "properties": {
            "keys": {
                "type": "array",
                "items": { "type": "string" },
                "x-shep-secret": true,
            },
            "labels": {
                "type": "object",
                "additionalProperties": { "type": "string" },
            },
        },
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "keys = [\"sk-live\"]\n".to_owned(),
    );
    for key in ["keys", "labels"] {
        assert_eq!(pane.lock(key), Some(Lock::NoWidget), "{key}");
    }
    pane.move_to_key("keys");
    pane.open_list();
    assert!(pane.list().is_none(), "no sub-screen over a secret list");
}

/// A dog's own schema nests a table inside a table the same way its
/// per-sheep one does, and `dogs.toml` spells the same data three ways:
/// a `[models.worker]` header, dotted keys, and an inline table.
#[test]
fn a_nested_table_in_a_dogs_own_schema_becomes_dotted_rows() {
    let schema = serde_json::json!({
        "$ref": "#/$defs/Settings",
        "$defs": {
            "Settings": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "models": { "$ref": "#/$defs/Models" },
                },
            },
            "Models": {
                "type": "object",
                "properties": { "worker": { "$ref": "#/$defs/Worker" } },
            },
            "Worker": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "token": { "type": "string", "x-shep-secret": true },
                },
            },
        },
    });
    for section in [
        "[models.worker]\nmodel = \"gpt\"\ntoken = \"sk-live-51Hx9Qa\"\n",
        "models.worker.model = \"gpt\"\nmodels.worker.token = \"sk-live-51Hx9Qa\"\n",
        "models = { worker = { model = \"gpt\", token = \"sk-live-51Hx9Qa\" } }\n",
    ] {
        let pane = ConfigPane::dog("jobs".into(), None, schema.clone(), section.to_owned());
        let keys: Vec<&str> = pane
            .fields()
            .fields()
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(
            keys,
            ["models.worker.model", "models.worker.token", "name"],
            "{section}"
        );
        assert_eq!(pane.value("models.worker.model"), "gpt", "{section}");
        assert_eq!(
            pane.value("models.worker.token"),
            "sk-live-51Hx9Qa",
            "{section}"
        );
        assert!(
            pane.fields()
                .by_key("models.worker.token")
                .expect("a row for the secret")
                .secret,
            "a secret one table down is still marked"
        );
        assert_eq!(pane.lock("models.worker.model"), None, "{section}");
    }
}

/// A secret row seeds its editor empty, however deep in the section the
/// credential sits: the pane renders `<set>` and must not hand the old
/// value back for a backspace to edit.
#[test]
fn a_secret_under_a_nested_table_seeds_its_editor_empty() {
    let schema = serde_json::json!({
        "properties": { "db": { "$ref": "#/$defs/Db" } },
        "$defs": {
            "Db": {
                "type": "object",
                "properties": {
                    "host": { "type": "string" },
                    "password": { "type": "string", "x-shep-secret": true },
                },
            },
        },
    });
    let mut pane = ConfigPane::dog(
        "watch".into(),
        None,
        schema,
        "[db]\nhost = \"h\"\npassword = \"ab12cd34\"\n".to_owned(),
    );
    pane.move_to_key("db.password");
    pane.begin_typing();
    assert_eq!(
        pane.typing().expect("the editor is open").buffer.as_str(),
        "",
        "a secret seeds empty, at any depth"
    );
    assert!(!pane.fields().by_key("db.host").expect("a row").secret);
}

/// A dog's section is the single most credential-dense thing this
/// screen loads: bark's own `sinks` table is a webhook URL with a
/// bearer token in it. The redaction is on the pane, not on each
/// type that carries the text.
#[test]
fn a_dog_panes_debug_names_no_section() {
    let pane = bark_pane();
    assert_eq!(
        format!("{pane:?}"),
        r#"ConfigPane { target: Dog { name: "bark", adopted_path: None }, fields: 5, env_keys: 0, cursor: 0 }"#
    );
}

/// `args` and `cwd` live in `values` and routinely carry a token or a
/// home directory (IR-41).
#[test]
fn the_panes_debug_names_no_value_it_holds() {
    let mut config = AppConfig {
        name: "web".into(),
        cwd: Some("/home/ada/secret-project".into()),
        args: vec!["--token".into(), "hunter2".into()],
        ..AppConfig::default()
    };
    config.env.insert("DB_PASSWORD".into(), "hunter2".into());
    let pane = ConfigPane::sheep(SheepConfigView::new(config, Vec::new(), Vec::new()));
    assert_eq!(
        format!("{pane:?}"),
        r#"ConfigPane { target: Sheep { name: "web" }, fields: 44, env_keys: 1, cursor: 0 }"#
    );
}
