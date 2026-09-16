//! The daemon's own records: JSON, escapes, and which levels survive.

use super::*;

#[cfg(unix)]
/// `SHEP_LOG_JSON=1` renders the daemon's own records as JSON, one object per
/// line, in the file `launch.rs` redirects its stderr into.
///
/// Every non-empty line is parsed, not only the one under test: a file where
/// one line in twenty is prose is not machine-readable.
#[test]
fn shep_log_json_makes_the_daemons_own_records_json() {
    let dir = tempfile::tempdir().unwrap();
    let log = daemon_log_after_a_missed_handshake(&dir, &[("SHEP_LOG_JSON", "1")]);

    let lines: Vec<&str> = log.lines().filter(|line| !line.trim().is_empty()).collect();
    assert!(
        !lines.is_empty(),
        "the daemon must have written something to read: {log:?}"
    );
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|err| {
                panic!("every line of shepd.err.log must be JSON under log_json: {line:?} ({err})")
            })
        })
        .collect();
    assert!(
        records.iter().any(|record| {
            record["level"] == "WARN"
                && record["fields"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(READINESS_RECORD))
        }),
        "the readiness record must survive as a JSON object with its level and \
         message intact: {records:?}"
    );
}

// --- Case 17 -------------------------------------------------------------

#[cfg(unix)]
/// `tracing_subscriber` defaults to colour whenever its `ansi` feature is
/// compiled in, so `install_log_subscriber`'s `.with_ansi(ansi_enabled(..))`
/// is what keeps escapes out of `shepd.err.log`. Without it they land mid
/// field name and break every substring assertion in this file.
#[test]
fn the_daemons_own_log_carries_no_ansi_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let log = daemon_log_after_a_missed_handshake(&dir, &[]);

    assert!(
        log.contains(READINESS_RECORD),
        "precondition: the daemon must have written a record to colour: {log:?}"
    );
    assert!(
        !log.contains('\x1b'),
        "a log file is not a terminal: {log:?}"
    );
}

#[cfg(unix)]
/// The same `WARN` record is written at the default level and filtered out at
/// `error`. Both halves provoke it on identical configuration, so the absent
/// half means filtered rather than never happened.
///
/// `error` rather than `off`: an `EnvFilter` built from an empty or
/// unparseable directive also degrades toward `off`, so silence alone would be
/// consistent with the level never being read.
#[test]
fn shep_log_level_decides_which_of_the_daemons_records_survive() {
    let at_default = tempfile::tempdir().unwrap();
    let default_log = daemon_log_after_a_missed_handshake(&at_default, &[]);
    assert!(
        default_log.contains(READINESS_RECORD),
        "a warn-level record must reach the log at the default level: {default_log:?}"
    );

    let at_error = tempfile::tempdir().unwrap();
    let error_log = daemon_log_after_a_missed_handshake(&at_error, &[("SHEP_LOG_LEVEL", "error")]);
    assert!(
        !error_log.contains(READINESS_RECORD),
        "SHEP_LOG_LEVEL=error must filter out the same warn-level record the \
         default level lets through: {error_log:?}"
    );
}
