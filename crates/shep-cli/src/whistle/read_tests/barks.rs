//! `list_barks`' tests: the history file read straight off disk.
//!
//! Split out of `read_tests.rs` under IR-48.

use super::*;

/// The alert history is on disk so it survives the shepherd; the case
/// this tool exists for is a model reading it after a crash.
///
/// The `Shepherd` handed in points at nothing listening, so a tool that
/// connected would fail rather than pass quietly.
#[tokio::test]
async fn list_barks_reads_the_file_with_no_shepherd_anywhere_in_reach() {
    let dir = tempfile::tempdir().unwrap();
    let barks_path = dir.path().join("barks.jsonl");
    let bark = shep_core::barks::Bark {
        at_ms: 1,
        rule: "restart-loop".to_string(),
        subject: "web".to_string(),
        message: "web restarted 5 times in 60s".to_string(),
        sinks: Vec::new(),
    };
    std::fs::write(
        &barks_path,
        format!("{}\n", serde_json::to_string(&bark).unwrap()),
    )
    .unwrap();

    // Nothing ever binds this socket.
    let unreachable_socket = shep_client::testing::control_address(dir.path());
    let whistle = whistle_at(unreachable_socket, barks_path);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_barks(Parameters(BarksParams { tail: None })),
    )
    .await
    .expect("list_barks must return within the test timeout")
    .expect("reading straight off disk must not fail");

    assert_eq!(result.0.barks.len(), 1);
    assert_eq!(result.0.barks[0].subject, "web");
}

/// One bark record whose only distinguishing field is when it fired,
/// so the `list_barks` tests below can read the order back off `at_ms`
/// instead of off a message they had to invent.
fn bark_at(at_ms: u64) -> shep_core::barks::Bark {
    shep_core::barks::Bark {
        at_ms,
        rule: "restart-loop".to_string(),
        subject: "web".to_string(),
        message: format!("web restarted {at_ms} times in 60s"),
        sinks: Vec::new(),
    }
}

/// Writes `records` as the bark history file, oldest line first.
fn write_barks(path: &Path, records: &[shep_core::barks::Bark]) {
    let text: String = records
        .iter()
        .map(|bark| format!("{}\n", serde_json::to_string(bark).unwrap()))
        .collect();
    std::fs::write(path, text).unwrap();
}

/// `list_barks` answers "what went wrong most recently". Returning the
/// oldest records answers a different question, and returning them
/// newest-first makes a model re-read the whole list to find the last.
#[tokio::test]
async fn list_barks_returns_the_newest_tail_asked_for_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let barks_path = dir.path().join("barks.jsonl");
    write_barks(&barks_path, &[bark_at(1), bark_at(2), bark_at(3)]);

    // Nothing listens here: `list_barks` reads the file alone.
    let whistle = whistle_at(
        shep_client::testing::control_address(dir.path()),
        barks_path,
    );
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_barks(Parameters(BarksParams { tail: Some(2) })),
    )
    .await
    .expect("list_barks must return within the test timeout")
    .expect("reading the tail of the history must not fail");

    let stamps: Vec<u64> = result.0.barks.iter().map(|row| row.at_ms).collect();
    assert_eq!(
        stamps,
        vec![2, 3],
        "the newest two, still in file order: {stamps:?}"
    );
}

/// `tail: 1000` is a model asking for more than a context window holds.
/// The clamp is what keeps the reply bounded, and it has to clamp
/// against the newest end: a cap that kept the oldest 200 alerts would
/// answer this question with the least recent half of the history.
#[tokio::test]
async fn list_barks_caps_the_tail_at_the_maximum() {
    let dir = tempfile::tempdir().unwrap();
    let barks_path = dir.path().join("barks.jsonl");
    let records: Vec<shep_core::barks::Bark> = (1..=205).map(bark_at).collect();
    write_barks(&barks_path, &records);

    let whistle = whistle_at(
        shep_client::testing::control_address(dir.path()),
        barks_path,
    );
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_barks(Parameters(BarksParams { tail: Some(1000) })),
    )
    .await
    .expect("list_barks must return within the test timeout")
    .expect("a request over the cap must be clamped, not refused");

    assert_eq!(
        result.0.barks.len(),
        200,
        "the 200 clamp must hold even against a request for 1000: {}",
        result.0.barks.len()
    );
    assert_eq!(
        result.0.barks.last().map(|row| row.at_ms),
        Some(205),
        "the clamped tail still ends on the newest alert"
    );
    assert_eq!(
        result.0.barks.first().map(|row| row.at_ms),
        Some(6),
        "and drops the oldest 5, not the newest"
    );
}
