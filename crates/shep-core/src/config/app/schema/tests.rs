use super::super::probe::ProbeKind;

// use schemars::generate

use super::*;
use crate::values::{MemSize, UpDuration};

use super::super::testing::*;

#[test]
fn minimal_config_gets_spec_defaults() {
    let app = AppConfig::minimal("web", "./server");
    assert_eq!(app.name, "web");
    assert_eq!(app.script, "./server");
    assert!(app.autorestart);
    assert!(app.autostart);
    assert_eq!(app.instances, 1);
    assert_eq!(app.min_uptime, UpDuration::from_millis(1000));
    assert_eq!(app.max_restarts, 16);
    assert_eq!(app.kill_timeout, UpDuration::from_millis(1600));
    assert_eq!(app.listen_timeout, UpDuration::from_millis(3000));
    assert_eq!(app.graceful_timeout, UpDuration::from_millis(8000));
    assert_eq!(app.action_timeout, UpDuration::from_millis(3000));
    assert!(app.max_memory.is_none());
    assert!(app.fold.is_none());
    assert!(!app.channel);
}

#[test]
fn unstable_restarts_are_throttled_by_default() {
    let app = AppConfig::minimal("web", "./srv");
    assert_eq!(
        app.exp_backoff_restart_delay,
        Some(UpDuration::from_millis(100))
    );
}

#[test]
fn stdin_is_not_piped_unless_the_app_asks() {
    let app = AppConfig::minimal("web", "./srv");
    assert!(!app.stdin);
    let parsed: AppConfig = toml::from_str("name = \"web\"\nscript = \"./srv\"").unwrap();
    assert!(!parsed.stdin);
}

#[test]
fn pty_is_not_set_unless_the_app_asks() {
    let app = AppConfig::minimal("web", "./srv");
    assert!(!app.pty);
    let parsed: AppConfig = toml::from_str("name = \"web\"\nscript = \"./srv\"").unwrap();
    assert!(!parsed.pty);
}

#[test]
fn the_flockfile_key_is_pty() {
    let parsed: AppConfig =
        toml::from_str("name = \"web\"\nscript = \"./srv\"\npty = true").unwrap();
    assert!(parsed.pty);
}

#[test]
fn the_flockfile_key_is_stdin() {
    let parsed: AppConfig =
        toml::from_str("name = \"web\"\nscript = \"./srv\"\nstdin = true").unwrap();
    assert!(parsed.stdin);
}

#[test]
fn environment_defaults_to_absent_and_parses_from_a_flockfile() {
    assert_eq!(AppConfig::default().environment, None);
    let app: AppConfig =
        toml::from_str("name = \"web\"\nscript = \"./srv\"\nenvironment = \"staging\"").unwrap();
    assert_eq!(app.environment.as_deref(), Some("staging"));
}

#[test]
fn toml_round_trip_with_newtypes() {
    let toml_src = r#"
name = "worker"
script = "python3"
args = ["job.py", "--fast"]
max_memory = "512M"
min_uptime = "5s"
fold = "backend"
env = { RUST_LOG = "info" }
"#;
    let app: AppConfig = toml::from_str(toml_src).unwrap();
    assert_eq!(app.max_memory, Some("512M".parse::<MemSize>().unwrap()));
    assert_eq!(app.min_uptime, UpDuration::from_millis(5000));
    assert_eq!(app.fold.as_deref(), Some("backend"));
    assert_eq!(app.env.get("RUST_LOG").map(String::as_str), Some("info"));
    assert_eq!(app.args, vec!["job.py", "--fast"]);
}

/// Raw TOML scalars in `env` read as their string form. `true` becomes
/// `"true"`, `8080` becomes `"8080"`, and quoted strings pass through.
/// A single test here covers the deserialization path; the
/// `flockfile` test that reads the same document through all four
/// formats covers the format dispatch.
#[test]
fn env_coerces_raw_scalars_to_their_string_form() {
    let src = r#"
name = "web"
script = "./srv"
env = { SOME_BOOL = true, PORT = 8080, NEG = -1, STR = "plain" }
"#;
    let app: AppConfig = toml::from_str(src).unwrap();
    assert_eq!(app.env["SOME_BOOL"], "true");
    assert_eq!(app.env["PORT"], "8080");
    assert_eq!(app.env["NEG"], "-1");
    assert_eq!(app.env["STR"], "plain");
}

/// A protocol-8 peer still serializes `increment_var`, which this build
/// no longer has. It must deserialize and be dropped rather than error,
/// since `MIN_SUPPORTED` is 8.
///
/// Fails if `deny_unknown_fields` comes back to this struct. The denial
/// lives in `Flockfile::parse` so a newer daemon can hand an older client
/// a config it does not fully understand.
#[test]
fn a_protocol_8_payload_carrying_increment_var_still_deserializes() {
    let src = r#"{ "name":"web","script":"./srv","increment_var":null }"#;
    let app = serde_json::from_str::<AppConfig>(src)
        .expect("a version-8 payload must not be refused for a field this build dropped");
    assert_eq!(app.name, "web");
    assert_eq!(app.script, "./srv");

    let populated = r#"{ "name":"web","script":"./srv","increment_var":"WORKER_ID" }"#;
    let app = serde_json::from_str::<AppConfig>(populated)
        .expect("a populated one is ignored too, not refused");
    assert_eq!(app.name, "web");
}

/// A float is refused rather than coerced. `f64` carries no trailing zero
/// and no written precision, so accepting one hands the process a value
/// the operator did not write. Quoting is the way to keep the text.
///
/// Asserted through JSON: a TOML error echoes the offending source line,
/// which would put the value in the message whatever serde said.
#[test]
fn env_refuses_a_float_because_its_written_form_would_not_survive() {
    let src = r#"{ "name":"web","script":"./srv","env":{ "RATIO": 1.10 } }"#;
    let err = serde_json::from_str::<AppConfig>(src)
        .expect_err("a float env value must be refused, not rounded")
        .to_string();
    assert!(
        err.contains("quote it"),
        "the error must name the fix, got: {err}"
    );
    assert!(!err.contains("1.1"), "the error leaked the value: {err}");

    let quoted = src.replace("1.10", r#""1.10""#);
    let app: AppConfig = serde_json::from_str(&quoted).unwrap();
    assert_eq!(app.env["RATIO"], "1.10");

    assert!(
        toml::from_str::<AppConfig>("name = \"web\"\nscript = \"./srv\"\nenv = { RATIO = 1.10 }\n")
            .is_err(),
        "TOML must refuse a float too"
    );
}

/// A whole number larger than `i64::MAX` is valid JSON and must load.
/// TOML cannot reach this test: its spec bounds integers to signed 64-bit,
/// so only JSON exercises this path. The value stringifies to its full
/// positive form, not a negative wrap. Pinned to the exact string.
#[test]
fn env_reads_a_number_beyond_i64_max_without_wrapping() {
    let beyond_i64 = u64::MAX; // 18446744073709551615
    let src = format!(r#"{{ "name":"web","script":"./srv","env":{{ "BIG": {beyond_i64} }} }}"#);
    let app = serde_json::from_str::<AppConfig>(&src)
        .expect("a u64 beyond i64::MAX is valid JSON and must load");
    assert_eq!(app.env["BIG"], "18446744073709551615");
}

/// Serialization is the inverse of the coercion: whatever form a value
/// arrived as, the wire form is a string. A `true` that deserialized
/// into `"true"` must serialize to the JSON string `"true"`, not the
/// boolean `true`.
#[test]
fn env_serialization_is_always_string_regardless_of_input_form() {
    let src = r#"
name = "web"
script = "./srv"
env = { SOME_BOOL = true, PORT = 8080, STR = "hello" }
"#;
    let app: AppConfig = toml::from_str(src).unwrap();
    let wire = serde_json::to_value(&app).unwrap();
    for (key, expected) in [("SOME_BOOL", "true"), ("PORT", "8080"), ("STR", "hello")] {
        assert_eq!(
            wire["env"].get(key).and_then(serde_json::Value::as_str),
            Some(expected),
            "wire form of {key} must be a string, not a scalar"
        );
    }
}

/// A value that is neither a string, boolean, nor number is refused, not
/// guessed at. An array or object under `env` is a structural mistake —
/// an operator meant a table or a list, and guessing a serialization is
/// how a wrong value hides for months.
#[test]
fn env_refuses_structural_values() {
    for (label, inner) in [("array", r#"["a", "b"]"#), ("object", r#"{"k": "v"}"#)] {
        let src = format!(r#"{{ "name":"web","script":"./srv","env":{{"X":{inner}}}}}"#);
        assert!(
            serde_json::from_str::<AppConfig>(&src).is_err(),
            "a {label} env value must be refused"
        );
    }
}

/// The wire path is the opposite of a Flockfile's: an unknown field
/// means a newer peer, and ignoring it is what stops a new Flockfile
/// field breaking an older client that reads a config off the wire.
/// `deny_unknown_fields` used to live here; the same typo is now
/// refused only at `Flockfile::parse`, where the input really is a
/// hand-written file.
#[test]
fn an_unknown_field_on_the_wire_is_ignored_rather_than_refused() {
    let config: AppConfig =
        serde_json::from_str(r#"{"name":"web","script":"./srv","invented_next_year":true}"#)
            .expect("the wire path tolerates what it does not know");
    assert_eq!(config.name, "web");
}

#[test]
fn probe_config_parses_with_defaults() {
    let src = r#"
name = "api"
script = "./api"

[readiness_probe]
kind = "http"
target = "http://127.0.0.1:8080/healthz"
"#;
    let app: AppConfig = toml::from_str(src).unwrap();
    let probe = app.readiness_probe.unwrap();
    assert_eq!(probe.kind, ProbeKind::Http);
    assert_eq!(probe.target, "http://127.0.0.1:8080/healthz");
    assert_eq!(probe.interval, UpDuration::from_millis(10_000));
    assert_eq!(probe.timeout, UpDuration::from_millis(5_000));
    assert_eq!(probe.failure_threshold, 3);
    assert!(app.liveness_probe.is_none());
}

#[test]
fn debug_redacts_env_values() {
    // Exact string pinned so a lazy derive(Debug) refactor fails here.
    let mut app = AppConfig::minimal("web", "./srv");
    app.env
        .insert("DATABASE_URL".to_string(), "postgres://secret".to_string());
    app.env.insert("RUST_LOG".to_string(), "info".to_string());
    assert_eq!(
        format!("{app:?}"),
        "AppConfig { name: \"web\", script: \"./srv\", env: <2 vars>, .. }"
    );
}

/// A dog's table can hold a credential the way env can; `AppConfig`'s
/// manual `Debug` omits `dogs` entirely via `finish_non_exhaustive`, so
/// a value inside one must never appear.
#[test]
fn debug_never_prints_a_dogs_table_value() {
    let mut app = AppConfig::minimal("web", "./srv");
    let mut table = serde_json::Map::new();
    table.insert(
        "token".to_string(),
        serde_json::Value::String("super-secret-token".to_string()),
    );
    app.dogs
        .insert("jobs".to_string(), crate::config::DogTable::from(table));
    assert!(!format!("{app:?}").contains("super-secret-token"));
}

/// The `AppConfig` property table of the Flockfile schema, owned because
/// the schema value is a local a borrow could not outlive.
fn app_props() -> serde_json::Map<String, serde_json::Value> {
    crate::config::flockfile_schema_json()
        .to_value()
        .pointer("/$defs/AppConfig/properties")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .expect("app config properties must exist")
}

#[test]
fn the_flockfile_schema_has_no_sandbox_property() {
    assert!(!app_props().contains_key("sandbox"));
}

#[test]
fn a_wire_config_with_the_sandbox_key_still_deserializes_and_is_refused_by_normalize() {
    let wire = serde_json::json!({
        "name": "web",
        "script": "./srv",
        "sandbox": "sandbox.json"
    });
    let app: AppConfig = serde_json::from_value(wire).unwrap();
    assert_eq!(app.sandbox.as_deref(), Some("sandbox.json"));
    assert_eq!(
        crate::config::normalize(app).unwrap_err(),
        crate::config::NormalizeError::SandboxKeyRemoved {
            name: "web".to_string()
        }
    );
}

#[test]
fn the_sandbox_key_is_never_serialized() {
    let app = AppConfig {
        sandbox: Some("sandbox.json".to_string()),
        ..AppConfig::minimal("web", "./srv")
    };
    let wire = serde_json::to_value(&app).unwrap();
    assert!(!wire.as_object().unwrap().contains_key("sandbox"));
}

/// The path fields are the ones an operator gets wrong, and the ones the
/// type table cannot describe. Each states what it takes.
#[test]
fn the_path_fields_state_what_they_accept() {
    let props = app_props();
    for name in ["cwd", "script", "out_file", "err_file"] {
        let accepts = props[name]["init"]["accepts"].as_array();
        assert!(
            accepts.is_some_and(|forms| !forms.is_empty()),
            "{name} carries no accepted forms"
        );
    }
}

/// Every entry carries both halves, so nothing renders half a line.
#[test]
fn every_neighbour_entry_carries_a_field_and_a_note() {
    let props = app_props();
    for (name, prop) in props {
        let Some(entries) = prop["init"]["neighbours"].as_array() else {
            continue;
        };
        for entry in entries {
            assert!(
                entry["field"].is_string(),
                "{name} has a neighbour with no field"
            );
            assert!(
                entry["note"].is_string(),
                "{name} has a neighbour with no note"
            );
        }
    }
}

/// fails if a field claims a refusal `normalize` does not make. The
/// static tables behind the same panel are parser-backed in
/// `shep-cli`'s `lookout::validation`; a per-field clause is only
/// backed here.
#[test]
fn every_per_field_refusal_is_one_normalize_really_makes() {
    for claim in refusal_claims() {
        let (value, matches) = match claim.proof {
            Proof::Refused { value, matches } => (value, matches),
            Proof::RefusedOnWindows { value, matches } if cfg!(windows) => (value, matches),
            Proof::RefusedOnWindows { .. } | Proof::Elsewhere(_) => continue,
        };
        let field = claim.field;
        let refusal = claim.refusal;
        match crate::config::normalize(*value) {
            Ok(_) => panic!("{field} says it refuses {refusal}, and normalize accepted it"),
            Err(err) => assert!(
                matches(&err),
                "{field}'s \"{refusal}\" was refused as {err:?}, which is not the variant \
                     the table names"
            ),
        }
    }
}

/// fails if a value the panel offers with one keypress is one
/// `normalize` turns around. A suggestion is a literal fill, so it is
/// the one claim a config can be checked against directly.
#[test]
fn every_suggested_value_is_one_normalize_accepts() {
    let props = app_props();
    for (field, prop) in props {
        let Some(values) = prop["init"]["suggest"].as_array() else {
            continue;
        };
        for value in values {
            let value = value.as_str().expect("a suggestion is a string");
            let app = sheep(|a| match field.as_str() {
                "cron_restart" => a.cron_restart = Some(value.to_owned()),
                "kill_signal" => a.kill_signal = Some(value.to_owned()),
                other => panic!("{other} suggests values that this test cannot place"),
            });
            assert!(
                crate::config::normalize(app).is_ok(),
                "{field} suggests `{value}`, which normalize refuses"
            );
        }
    }
}

/// fails if the schema and the table have drifted apart in either
/// direction: a new clause with no proof, or a proof for a clause no
/// field writes any more.
#[test]
fn the_refusal_table_and_the_schema_name_the_same_clauses() {
    let props = app_props();
    let in_schema: Vec<(&str, &str)> = props
        .iter()
        .filter_map(|(field, prop)| Some((field.as_str(), prop["init"]["refuses"].as_array()?)))
        .flat_map(|(field, clauses)| {
            clauses.iter().map(move |clause| {
                (
                    field,
                    clause.as_str().expect("a refusal clause is a string"),
                )
            })
        })
        .collect();

    let claims = refusal_claims();
    let proved: Vec<(&str, &str)> = claims.iter().map(|c| (c.field, c.refusal)).collect();

    let unproved: Vec<_> = in_schema
        .iter()
        .filter(|clause| !proved.contains(clause))
        .collect();
    let stale: Vec<_> = proved
        .iter()
        .filter(|clause| !in_schema.contains(clause))
        .collect();
    assert!(
        unproved.is_empty() && stale.is_empty(),
        "a refusal and its proof live together, in refusal_claims in this file.\n  \
             claimed by a field, proved nowhere: {unproved:?}\n  \
             proved here, claimed by no field: {stale:?}"
    );
}
