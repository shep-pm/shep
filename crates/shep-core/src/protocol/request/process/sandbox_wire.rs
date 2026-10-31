//! The wire shape of a row's `sandbox_refused` flag: absent from the payload
//! until a daemon has an `errored` sheep to say something about.

use super::*;

#[test]
fn a_sandbox_refusal_flag_is_absent_from_the_wire_until_set() {
    let plain = ProcessInfo::builder(1, "web", ProcStatus::Errored).build();
    let json = serde_json::to_string(&plain).expect("serialize");
    assert!(!json.contains("sandbox_refused"), "{json}");

    for flag in [true, false] {
        let row = ProcessInfo::builder(1, "web", ProcStatus::Errored)
            .sandbox_refused(Some(flag))
            .build();
        let json = serde_json::to_string(&row).expect("serialize");
        assert!(
            json.contains(&format!(r#""sandbox_refused":{flag}"#)),
            "{json}"
        );
        let back: ProcessInfo = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.sandbox_refused, Some(flag));
    }
}

#[test]
fn an_errored_row_from_a_daemon_without_the_flag_decodes_as_unknown() {
    let older = r#"{"id":1,"name":"web","status":"errored","restarts":16,"uptime_ms":0}"#;
    let info: ProcessInfo = serde_json::from_str(older).expect("an older payload decodes");
    assert_eq!(info.sandbox_refused, None);
}
