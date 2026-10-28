//! The wire shape of a row's `vcs`: no key at all until a shepherd has a
//! revision to report, and a nested object naming one when it has.
//!
//! Sits beside [`super`] rather than inside it because `process.rs` is at
//! IR-48's file-size ceiling, and this is the field that has to grow there.

use super::*;

/// A row whose shepherd has no revision to report is the bytes it always was:
/// no key, not a `null`.
#[test]
fn a_row_with_no_revision_has_no_vcs_key() {
    let plain = ProcessInfo::builder(1, "web", ProcStatus::Online).build();
    let json = serde_json::to_string(&plain).expect("serialize");

    assert!(
        !json.contains("vcs"),
        "a row with nothing to say carries no key at all: {json}"
    );
    let back: ProcessInfo = serde_json::from_str(&json).expect("round trip");
    assert_eq!(back.vcs, None);
}

/// A detached head names the revision and no branch; a checkout on a branch
/// names both. Each survives the trip back unchanged.
#[test]
fn a_revision_round_trips_with_and_without_a_branch() {
    let detached = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .vcs(Some(Vcs::new(REVISION)))
        .build();
    let json = serde_json::to_string(&detached).expect("serialize");
    let shape = format!(r#""vcs":{{"revision":"{REVISION}","branch":null}}"#);
    assert!(json.contains(&shape), "a detached head: {json}");
    assert_eq!(
        serde_json::from_str::<ProcessInfo>(&json).expect("round trip"),
        detached
    );

    let on_a_branch = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .vcs(Some(Vcs::new(REVISION).with_branch("main")))
        .build();
    let json = serde_json::to_string(&on_a_branch).expect("serialize");
    assert!(
        json.contains(&format!(
            r#""vcs":{{"revision":"{REVISION}","branch":"main"}}"#
        )),
        "a checkout on a branch names it: {json}"
    );
    assert_eq!(
        serde_json::from_str::<ProcessInfo>(&json).expect("round trip"),
        on_a_branch
    );
}

/// A client built before this field existed still reads the row it rides in:
/// `ProcessInfo` carries no `deny_unknown_fields`, unlike the config types in
/// `crate::config`.
#[test]
fn an_older_client_ignores_the_vcs_key() {
    #[derive(Deserialize)]
    struct BeforeVcs {
        id: u32,
        name: String,
        lambs: Option<Vec<Lamb>>,
    }

    let current = ProcessInfo::builder(1, "web", ProcStatus::Online)
        .lambs(Some(vec![Lamb::new(4243, "node")]))
        .vcs(Some(Vcs::new(REVISION).with_branch("main")))
        .build();
    let json = serde_json::to_string(&current).expect("serialize");

    let older: BeforeVcs = serde_json::from_str(&json).expect("an older client reads the row");
    assert_eq!(older.id, 1);
    assert_eq!(older.name, "web");
    assert_eq!(older.lambs, Some(vec![Lamb::new(4243, "node")]));
}
