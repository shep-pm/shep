//! A row's own tests: the builder's defaults, every setter writing its own
//! field, and what a payload from an older peer decodes to.
//!
//! Sits beside [`super`] rather than inside it because `process.rs` is at
//! IR-48's file-size ceiling. Its doc links are not checked by anything:
//! `#[cfg(test)]` modules are outside `cargo doc`.

use super::*;

#[test]
fn a_spawn_failure_is_additive_and_round_trips_verbatim() {
    let plain = ProcessInfo::builder(1, "kelpie", ProcStatus::Errored).build();
    let json = serde_json::to_value(&plain).unwrap();
    assert!(json.get("spawn_failure").is_none());
    assert_eq!(
        serde_json::from_value::<ProcessInfo>(json)
            .unwrap()
            .spawn_failure,
        None
    );

    let info = ProcessInfo::builder(1, "kelpie", ProcStatus::Errored)
        .spawn_failure(Some(
            "kelpie: process spawn failed: permission denied".to_string(),
        ))
        .build();
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(
        json["spawn_failure"],
        "kelpie: process spawn failed: permission denied"
    );
    assert_eq!(serde_json::from_value::<ProcessInfo>(json).unwrap(), info);
}

#[test]
fn a_builder_with_nothing_set_is_a_sheep_that_has_not_run() {
    let info = ProcessInfo::builder(3, "web", ProcStatus::Stopped).build();

    assert_eq!(info.id, 3);
    assert_eq!(info.name, "web");
    assert_eq!(info.status, ProcStatus::Stopped);
    assert_eq!(info.pid, None);
    assert_eq!(info.restarts, 0);
    assert_eq!(info.uptime_ms, 0);
    assert_eq!(info.fold, None);
    assert_eq!(info.out_file, None);
    assert_eq!(info.err_file, None);
    assert_eq!(info.cpu_percent, None);
    assert_eq!(info.memory_bytes, None);
    assert_eq!(info.cpu_ms, None);
    assert_eq!(info.dog, None);
    assert_eq!(info.lambs, None);
    assert_eq!(info.last_exit, None);
}

/// The raw counter rides the wire beside the percent. A client polling
/// faster than the shepherd's own sampling interval differences two of
/// these; `cpu_percent` cannot serve it, because consecutive readings
/// share a baseline.
#[test]
fn a_process_info_carries_the_cpu_counter() {
    let info = ProcessInfo::builder(3, "web", ProcStatus::Online)
        .cpu_ms(Some(1_234))
        .build();
    assert_eq!(info.cpu_ms, Some(1_234));
}

/// Absent by default, like every other sampled field: a lifecycle verb's
/// answer carries no reading.
#[test]
fn a_process_info_without_a_reading_has_no_counter() {
    let info = ProcessInfo::builder(3, "web", ProcStatus::Online).build();
    assert_eq!(info.cpu_ms, None);
}

/// A status a newer shepherd adds must not fail the row it rides in.
#[test]
fn a_process_info_with_a_status_this_build_does_not_know_still_decodes() {
    let mut wire = serde_json::to_value(sample_info()).unwrap();
    wire["status"] = serde_json::json!("invented_next_year");

    let decoded: ProcessInfo = serde_json::from_value(wire).unwrap();

    assert_eq!(decoded.status, ProcStatus::Unrecognized);
}

/// Every field is given a value distinct from every other field's
/// default, so a copy-pasted setter body shows up as a mismatch.
#[test]
fn every_setter_writes_its_own_field_and_no_other() {
    let built = ProcessInfo::builder(3, "web", ProcStatus::Online)
        .pid(Some(4242))
        .restarts(1)
        .uptime_ms(60_000)
        .fold(Some("backend".to_string()))
        .out_file(Some("/home/ada/.shep/logs/web-0-out.log".to_string()))
        .err_file(Some("/home/ada/.shep/logs/web-0-err.log".to_string()))
        .cpu_percent(Some(12.5))
        .memory_bytes(Some(48 * 1024 * 1024))
        .dog(None)
        .last_exit(Some(ExitInfo {
            code: Some(1),
            signal: None,
        }))
        .max_memory(Some(512 * 1024 * 1024))
        .level_rules(vec![crate::config::LevelRule {
            pattern: r"\[ERROR\]".to_string(),
            level: crate::config::LineLevel::Error,
        }])
        .build();

    // `sample_info()` is a struct literal on purpose: it is the one
    // place that names every field by hand, so this comparison fails the
    // day the struct grows a field the builder cannot set.
    assert_eq!(built, sample_info());

    // `sample_info()`'s `dog` is `None`, the builder's default too, so an
    // empty `dog` setter body would pass the comparison above. It cannot
    // be changed: it feeds pinned snapshots.
    assert_eq!(
        ProcessInfo::builder(1, "metrics", ProcStatus::Online)
            .dog(Some(DogSource::BuiltIn { grants: None }))
            .build()
            .dog,
        Some(DogSource::BuiltIn { grants: None }),
        "an empty `dog` setter body is invisible to the comparison above"
    );

    // `lambs`, on `dog`'s terms above.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .lambs(Some(vec![Lamb::new(4243, "node")]))
            .build()
            .lambs,
        Some(vec![Lamb::new(4243, "node")]),
        "an empty `lambs` setter body is invisible to the comparison above"
    );

    // `smit`, on the same terms, and the field a third party writes: an
    // empty setter body drops every dog's mark.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .smit(Some("\u{25b2} main@a1b2c3".to_string()))
            .build()
            .smit
            .as_deref(),
        Some("\u{25b2} main@a1b2c3"),
        "an empty `smit` setter body is invisible to the comparison above"
    );

    // `handshook`, on the same terms.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .handshook(Some(false))
            .build()
            .handshook,
        Some(false),
        "an empty `handshook` setter body is invisible to the comparison above"
    );

    // `dog_stale`, paired with `handshook`: both default to `None`.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .dog_stale(Some(true))
            .build()
            .dog_stale,
        Some(true),
        "an empty `dog_stale` setter body is invisible to the comparison above"
    );

    // `pending`, on the same terms.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .pending(Some(vec!["env".to_string()]))
            .build()
            .pending,
        Some(vec!["env".to_string()]),
        "an empty `pending` setter body is invisible to the comparison above"
    );

    // `overridden`, on the same terms.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .overridden(Some(vec!["cwd".to_string()]))
            .build()
            .overridden,
        Some(vec!["cwd".to_string()]),
        "an empty `overridden` setter body is invisible to the comparison above"
    );

    // `sandbox_refused`, on the same terms.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Errored)
            .sandbox_refused(Some(true))
            .build()
            .sandbox_refused,
        Some(true),
        "an empty `sandbox_refused` setter body is invisible to the comparison above"
    );

    // `vcs`, on the same terms.
    assert_eq!(
        ProcessInfo::builder(1, "web", ProcStatus::Online)
            .vcs(Some(Vcs::new(REVISION).with_branch("main")))
            .build()
            .vcs,
        Some(Vcs::new(REVISION).with_branch("main")),
        "an empty `vcs` setter body is invisible to the comparison above"
    );
}

#[test]
fn lambs_distinguishes_not_walked_from_walked_and_empty() {
    let not_walked = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    assert_eq!(not_walked.lambs, None);

    let walked_empty = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .lambs(Some(Vec::new()))
        .build();
    assert_eq!(walked_empty.lambs, Some(Vec::new()));
}

#[test]
fn a_process_info_without_a_lambs_key_still_deserializes() {
    let fixture = r#"{
        "id": 3, "name": "web", "status": "online", "pid": 4242,
        "restarts": 0, "uptime_ms": 100, "fold": null,
        "out_file": null, "err_file": null,
        "cpu_percent": null, "memory_bytes": null, "dog": null
    }"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.lambs, None);
}

/// argv holds credentials (`--password=`, `?token=`) and
/// `shep describe --format json` is output people paste into issues.
#[test]
fn a_lamb_is_a_pid_and_an_executable_name() {
    let lamb = Lamb::new(4243, "node");
    let json = serde_json::to_string(&lamb).unwrap();
    assert_eq!(json, r#"{"pid":4243,"name":"node"}"#);
    assert_eq!(serde_json::from_str::<Lamb>(&json).unwrap(), lamb);
}

#[test]
fn a_labelled_lamb_carries_the_label_beside_the_name() {
    let lamb = Lamb::new(4243, "python").with_label("worker 1");
    let json = serde_json::to_string(&lamb).unwrap();
    assert_eq!(json, r#"{"pid":4243,"name":"python","label":"worker 1"}"#);
    assert_eq!(serde_json::from_str::<Lamb>(&json).unwrap(), lamb);
}

#[test]
fn v1_process_info_without_a_dog_marker_still_deserializes() {
    let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log","cpu_percent":12.5,"memory_bytes":50331648}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.dog, None);
}

/// No field here carries `#[serde(default)]`: serde's derive resolves a
/// missing key to `None` for a field whose type is syntactically
/// `Option<...>`.
#[test]
fn a_process_info_without_a_last_exit_key_still_deserializes() {
    let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log","cpu_percent":12.5,"memory_bytes":50331648,"dog":null,"lambs":null}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.last_exit, None);
}

#[test]
fn a_process_info_without_a_smit_key_still_deserializes() {
    let fixture = r#"{"id":1,"name":"web","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":null,"lambs":null,"last_exit":null}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.smit, None);
}

/// The fixture is a dog's row, where `None` means "render this as it
/// rendered before the field existed", never "never handshaken".
#[test]
fn a_process_info_without_a_handshook_key_still_deserializes() {
    let fixture = r#"{"id":1,"name":"metrics","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":{"kind":"built_in"},"lambs":null,"last_exit":null,"smit":null,"instance":0}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.handshook, None);
    assert_eq!(info.dog, Some(DogSource::BuiltIn { grants: None }));
}

/// The fixture carries `handshook: false`, the case that matters: `None`
/// is "no verdict to report", never "it has not given up".
#[test]
fn a_process_info_without_a_dog_stale_key_still_deserializes() {
    let fixture = r#"{"id":1,"name":"metrics","status":"online","pid":42,"restarts":0,"uptime_ms":10,"fold":null,"out_file":null,"err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":{"kind":"built_in"},"lambs":null,"last_exit":null,"smit":null,"instance":0,"handshook":false}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.dog_stale, None);
    assert_eq!(info.handshook, Some(false));
}

#[test]
fn a_process_info_carries_its_memory_ceiling_and_defaults_to_none() {
    let plain = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    assert_eq!(
        plain.max_memory, None,
        "a sheep with no ceiling reports none"
    );

    let capped = ProcessInfo::builder(2, "hungry", ProcStatus::Online)
        .max_memory(Some(52 * 1024 * 1024))
        .build();
    assert_eq!(capped.max_memory, Some(54_525_952));
}

/// Absent rather than `null` when there is no reload, so a listing's
/// payload is the shape it always was and `SCHEMA_VERSION` stays put.
/// Present as a plain number when a reload sets one, which is the only
/// place a client reads it.
#[test]
fn a_reload_deadline_is_absent_from_the_wire_until_a_reload_sets_one() {
    let quiet = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    let json = serde_json::to_string(&quiet).expect("serialize");
    assert!(
        !json.contains("reload_deadline_ms"),
        "a row with no reload carries no key at all: {json}"
    );

    let reloading = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .reload_deadline_ms(Some(16_000))
        .build();
    let json = serde_json::to_string(&reloading).expect("serialize");
    assert!(
        json.contains(r#""reload_deadline_ms":16000"#),
        "a reload's own row carries the number: {json}"
    );
    let back: ProcessInfo = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back.reload_deadline_ms, Some(16_000));
}

/// A disabled sheep stays `stopped` on the wire and gains a key beside it;
/// every other row is the bytes it always was.
#[test]
fn a_disabled_flag_is_absent_from_the_wire_until_set() {
    let plain = ProcessInfo::builder(1, "web", ProcStatus::Stopped).build();
    let json = serde_json::to_string(&plain).expect("serialize");
    assert!(!json.contains("disabled"), "{json}");

    let parked = ProcessInfo::builder(1, "web", ProcStatus::Stopped)
        .disabled(Some(true))
        .build();
    let json = serde_json::to_string(&parked).expect("serialize");
    assert!(json.contains(r#""status":"stopped""#), "{json}");
    assert!(json.contains(r#""disabled":true"#), "{json}");
    let back: ProcessInfo = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back.disabled, Some(true));
}

#[test]
fn an_older_daemons_process_info_still_decodes() {
    // Every additive field, asserted off one payload. A payload written
    // before any of them existed has to decode with each absent rather
    // than fail the whole envelope, and one fixture cannot drift from
    // another.
    let older = r#"{"id":1,"name":"web","status":"online","restarts":0,"uptime_ms":0}"#;
    let info: ProcessInfo = serde_json::from_str(older).expect("an older payload decodes");
    assert_eq!(info.max_memory, None);
    assert_eq!(info.reload_deadline_ms, None);
    assert_eq!(info.disabled, None);
    assert_eq!(info.log_timestamps, None);
    assert_eq!(info.sandbox_refused, None);
    assert_eq!(info.vcs, None);
}

#[test]
fn v1_process_info_without_stats_still_deserializes() {
    let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend","out_file":"/l/o.log","err_file":"/l/e.log"}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.cpu_percent, None);
    assert_eq!(info.memory_bytes, None);
}

#[test]
fn v1_process_info_without_log_paths_still_deserializes() {
    // Committed byte fixture from before `out_file`/`err_file` existed.
    let fixture = r#"{"id":3,"name":"web","status":"online","pid":4242,"restarts":1,"uptime_ms":60000,"fold":"backend"}"#;
    let info: ProcessInfo = serde_json::from_str(fixture).unwrap();
    assert_eq!(info.id, 3);
    assert_eq!(info.out_file, None);
    assert_eq!(info.err_file, None);
}

#[test]
fn an_old_client_still_decodes_a_new_process_info() {
    // `ProcessInfo` carries no `deny_unknown_fields`, unlike the config
    // types in `crate::config`, so extra keys are ignored.
    #[derive(Deserialize)]
    struct V1ProcessInfo {
        id: u32,
        fold: Option<String>,
    }

    let current = serde_json::to_string(&sample_info()).unwrap();
    let old: V1ProcessInfo = serde_json::from_str(&current).unwrap();
    assert_eq!(old.id, 3);
    assert_eq!(old.fold.as_deref(), Some("backend"));
}

/// The fixture cannot agree under either candidate order: by id it is
/// `web/1, api/2, web/0`, by name `api, web, web`. The two `web` rows
/// are the tiebreak half, seeded out of order.
#[test]
fn a_listing_sorts_by_name_then_by_id() {
    let mut listing = vec![
        ProcessInfo::builder(1, "web", ProcStatus::Online).build(),
        ProcessInfo::builder(2, "api", ProcStatus::Online).build(),
        ProcessInfo::builder(0, "web", ProcStatus::Online).build(),
    ];
    sort_flock(&mut listing);

    let seen: Vec<(&str, u32)> = listing
        .iter()
        .map(|info| (info.name.as_str(), info.id))
        .collect();
    assert_eq!(
        seen,
        vec![("api", 2), ("web", 0), ("web", 1)],
        "name first, then id inside a name"
    );
}

#[test]
fn an_instance_slot_survives_a_round_trip_and_defaults_to_absent() {
    let with = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .instance(Some(2))
        .build();
    assert_eq!(with.instance, Some(2));

    let without = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    assert_eq!(
        without.instance, None,
        "a row nobody set a slot on says so, rather than claiming slot 0"
    );
}

#[test]
fn a_reply_from_a_daemon_without_the_field_deserializes_as_absent() {
    let json = r#"{"id":1,"name":"web","status":"online","pid":null,
        "restarts":0,"uptime_ms":0,"fold":null,"out_file":null,
        "err_file":null,"cpu_percent":null,"memory_bytes":null,"dog":null,
        "lambs":null,"last_exit":null,"smit":null}"#;
    let info: ProcessInfo = serde_json::from_str(json).expect("older reply still parses");
    assert_eq!(info.instance, None);
}
