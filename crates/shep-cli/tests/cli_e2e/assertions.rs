//! Normalising a reply before it is compared against a committed fixture,
//! and the comparisons themselves.

use super::*;

/// Asserts `info`, one `data[]` element of an envelope, carries the dynamic
/// fields a real spawned sheep must have, then blanks them to `null` so the
/// rest can be compared against a committed fixture verbatim.
///
/// `pid`, `uptime_ms` and the tempdir-rooted `out_file`/`err_file` cannot be
/// pinned across runs, so each is asserted against its own shape first.
/// `samples` says whether this verb takes a live resource reading, which is
/// the assertion `memory_bytes` gets in place of a value.
pub(crate) fn normalize_process_info(
    info: &mut serde_json::Value,
    home: &Path,
    name: &str,
    samples: Samples,
) {
    let pid = info["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("pid must be a real positive OS pid: {info}"));
    assert!(pid > 0, "pid must be a real positive OS pid: {info}");
    info["uptime_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("uptime_ms must be present: {info}"));
    let home_str = home.to_str().unwrap();
    for (key, stream) in [("out_file", "out"), ("err_file", "err")] {
        let path = info[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key} must be a string: {info}"));
        assert!(
            path.starts_with(home_str),
            "{key} must be rooted under $SHEP_HOME: {path}"
        );
        assert!(
            path.ends_with(&format!("{name}-0-{stream}.log")),
            "{key} must name this sheep's own log file: {path}"
        );
    }
    match samples {
        Samples::Live => {
            let bytes = info["memory_bytes"].as_u64().unwrap_or_else(|| {
                panic!("memory_bytes must be a live reading off the host: {info}")
            });
            assert!(
                bytes > 0,
                "a running sheep's tree cannot be 0 bytes: {info}"
            );
            info["cpu_ms"]
                .as_u64()
                .unwrap_or_else(|| panic!("cpu_ms must be a live reading off the host: {info}"));
        }
        Samples::None => {
            assert!(
                info["memory_bytes"].is_null(),
                "a verb that takes no live sample must report no memory: {info}"
            );
            assert!(
                info["cpu_ms"].is_null(),
                "a verb that takes no live sample must report no CPU counter: {info}"
            );
        }
    }
    // `cpu_percent` needs a periodic baseline, so whether one exists depends on
    // the daemon living through a poll interval: a clock race either way.
    info["pid"] = serde_json::Value::Null;
    info["uptime_ms"] = serde_json::Value::Null;
    info["out_file"] = serde_json::Value::Null;
    info["err_file"] = serde_json::Value::Null;
    info["cpu_percent"] = serde_json::Value::Null;
    info["memory_bytes"] = serde_json::Value::Null;
    info["cpu_ms"] = serde_json::Value::Null;
    // `lambs[].pid` races the same way. `lambs[].name` stays: it is
    // deterministic once the walk has caught the lamb.
    if let Some(lambs) = info["lambs"].as_array_mut() {
        for lamb in lambs {
            lamb["pid"] = serde_json::Value::Null;
        }
    }
}

/// Nulls every value under `host`, having first checked the two that are
/// never absent.
///
/// The figures themselves are this machine's, and the three rates depend on
/// how long the shepherd had been up when the listing arrived, so none of
/// them can be committed. The key set can: the fixture pins that `host` is
/// there at all and pins each field's name, so a renamed, dropped or
/// misspelled one fails here rather than reaching an operator's script.
///
/// A no-op where the verb carries no `host` key, which is every verb but
/// `flock`; the fixture pins the absence for those.
pub(crate) fn normalize_host(envelope: &mut serde_json::Value) {
    let Some(host) = envelope.get_mut("host") else {
        return;
    };
    let Some(host) = host.as_object_mut() else {
        panic!("host must be an object on a machine sysinfo can read: {host}");
    };
    for key in ["memory_used_bytes", "memory_total_bytes"] {
        let bytes = host[key]
            .as_u64()
            .unwrap_or_else(|| panic!("{key} is not a rate and is never absent: {host:?}"));
        assert!(bytes > 0, "{key} must be a live reading off the host");
    }
    for value in host.values_mut() {
        *value = serde_json::Value::Null;
    }
}

/// Whether the verb an envelope answers takes a live resource reading.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Samples {
    /// `flock` and `describe`, which sample the host as they reply.
    Live,
    /// Every other verb answering with a `ProcessInfo`.
    None,
}

/// Parses `output.stdout` as a `flock`/`describe`/`start` envelope,
/// normalizes its one `data[]` element, and compares the result against the
/// committed fixture named `command`.
pub(crate) fn assert_envelope_matches_fixture(
    output: &Output,
    home: &Path,
    command: &str,
    sheep_name: &str,
    samples: Samples,
) {
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "{command}: stdout was not JSON: {e}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        });
    {
        let data = envelope["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{command}: data must be an array"));
        assert_eq!(data.len(), 1, "{command}: exactly one sheep is expected");
    }
    normalize_process_info(&mut envelope["data"][0], home, sheep_name, samples);
    normalize_host(&mut envelope);
    assert_eq!(
        envelope,
        load_fixture(command),
        "{command} envelope drifted from its committed fixture"
    );
}

/// Asserts a failed `--format json` invocation kept `stdout` empty and put a
/// parseable `{"schema_version", "error": {"code", "message"}}` object on
/// `stderr`. Only this tier has two real streams.
pub(crate) fn assert_json_error(output: &Output, expected_status: i32, expected_error_code: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on failure: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let err: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr was not JSON: {e}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(err["error"]["code"], expected_error_code, "{err}");
}

/// Spells a path the way shep spells it: canonicalized, with Windows' `\?\`
/// prefix stripped back off so `shep.toml` stays hand-editable, and 8.3 short
/// names expanded (`%TEMP%` on a Windows runner is `C:\Users\RUNNER~1\...`,
/// which canonicalizes to `runneradmin`).
pub(crate) fn as_shep_spells_it(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("canonicalize the recorded binary");
    shep_core::paths::strip_verbatim_prefix(&canonical)
        .display()
        .to_string()
}

/// A log file's contents with the daemon's per-line timestamp taken back off,
/// through `shep_core::logstamp::strip`, the same call `shep bleats` makes.
pub(crate) fn unstamped_file(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(shep_core::logstamp::strip(line));
        out.push('\n');
    }
    out
}
