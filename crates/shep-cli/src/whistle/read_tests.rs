//! `read`'s tests: the five read tools, the local socket, and a host named
//! out of `shep.toml`'s `hosts` list.
//!
//! Split out of `read.rs` under IR-48. Reached as `read::tests` through the
//! `#[path]` declaration at the end of that file, so every test keeps the
//! path a caller cites it by.

use std::time::Duration;

use shep_core::protocol::{DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use super::*;
use crate::whistle::matching_ack;

#[cfg(unix)]
#[path = "read_tests/ssh_stand_in.rs"]
mod ssh_stand_in;
#[cfg(unix)]
use ssh_stand_in::{
    ssh_stand_in, ssh_stand_in_answering, ssh_stand_in_replying, ssh_stand_in_results,
    stand_in_file,
};
#[path = "read_tests/barks.rs"]
mod barks;
#[path = "read_tests/whistles.rs"]
mod whistles;
use whistles::whistle_at;
#[cfg(unix)]
use whistles::whistle_with_hosts;

/// How long a test waits for a tool call before treating it as hung.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// `shep flock` prints dogs in the same table; a model asking what is
/// running must see the same population.
#[tokio::test]
async fn list_flock_returns_every_registered_entry_including_dogs() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let sheep = shep_client::testing::sample_info();
    let dog = ProcessInfo::builder(2, "metrics", ProcStatus::Online)
        .dog(Some(DogSource::BuiltIn { grants: None }))
        .build();
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![sheep, dog]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams { host: None })),
    )
    .await
    .expect("list_flock must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    let names: Vec<&str> = result.0.flock.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["web", "metrics"],
        "every registered entry must come back, dogs included: {names:?}"
    );
    assert!(
        result.0.flock[1].dog.is_some(),
        "the dog row must carry its DogSourceRow: {:?}",
        result.0.flock[1]
    );

    served.await.expect("the fake daemon task must not panic");
}

/// Asserts on the request the fake daemon received, not the reply:
/// `SelectorSpec::Name("all")`, never `SelectorSpec::All`.
#[tokio::test]
async fn describe_sheep_never_builds_anything_but_a_name_selector() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Described(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.describe_sheep(Parameters(SheepName {
            name: "all".to_string(),
            host: None,
        })),
    )
    .await
    .expect("describe_sheep must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");
    assert_eq!(result.0.flock.len(), 1);

    let envelope = served.await.expect("the fake daemon task must not panic");
    match envelope.body {
        Request::Describe { selector } => assert_eq!(
            selector,
            SelectorSpec::Name("all".to_string()),
            "a literal name, never `SelectorSpec::All`: {selector:?}"
        ),
        other => panic!("expected Request::Describe, got {other:?}"),
    }
}

/// A host no operator listed is refused out of the list, and the stand-in
/// that would have been ssh is the proof: it records being run, and here it
/// was not run.
///
/// The failure this pins is the dangerous one. A `host` a model invented
/// that reached ssh would be a destination nobody chose, dialled from the
/// machine whistle was launched on.
#[cfg(unix)]
#[tokio::test]
async fn list_flock_with_an_unlisted_host_is_refused_before_ssh_runs() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in(
        dir.path(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2", "amp3"], &program);

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams {
            host: Some("build-01".to_string()),
        })),
    )
    .await
    .expect("list_flock must return within the test timeout");
    let refusal = match outcome {
        Ok(_) => panic!("a host no operator listed is a refusal, not a flock"),
        Err(refusal) => refusal,
    };

    assert_eq!(refusal.is_error, Some(true));
    let structured = refusal
        .structured_content
        .expect("a refusal carries structured content a model can branch on");
    assert_eq!(
        structured["code"], "unknown_host",
        "a distinct code, so a model can tell this from a host that was \
         unreachable: {structured}"
    );
    let message = structured["message"]
        .as_str()
        .expect("a string")
        .to_string();
    for listed in ["amp2", "amp3"] {
        assert!(
            message.contains(listed),
            "the refusal names the hosts the operator did list, since one of \
             them is what the caller most likely meant: {message}"
        );
    }
    assert!(
        !stand_in_file(dir.path(), "args.txt").exists(),
        "the stand-in ssh wrote no record, so nothing was dialled"
    );
}

/// A listed host is dialled, and the answer is that host's flock, not
/// this machine's: nothing is bound at the local socket in this test, so a
/// call that went local would fail with "no shepherd is running" rather
/// than answer with `queue`.
#[cfg(unix)]
#[tokio::test]
async fn list_flock_with_a_listed_host_answers_from_that_shepherd_and_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut remote = shep_client::testing::sample_info();
    remote.name = "queue".to_string();
    let program = ssh_stand_in(dir.path(), Response::Flock(vec![remote]));
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("list_flock must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    let names: Vec<&str> = result.0.flock.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["queue"],
        "the flock on the host answers, not this machine's: {names:?}"
    );
    assert_eq!(
        result.0.host.as_deref(),
        Some("amp2"),
        "and the reply says which shepherd it came from, spelled as the \
         caller asked for it: {:?}",
        result.0.host
    );
}

/// ssh is never allowed to ask anything. A tool call has no terminal
/// behind it and nobody watching it: a passphrase or host-key prompt would
/// sit until the timeout, and the model would read a timeout as a slow host
/// instead of the one-line refusal ssh gives when prompts are off.
///
/// The whole command line is pinned, not just the option: `--` is what
/// keeps a destination from being read as an ssh option.
#[cfg(unix)]
#[tokio::test]
async fn a_host_call_never_prompts() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in(dir.path(), Response::Flock(Vec::new()));
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("list_flock must return within the test timeout")
    .expect("the stand-in must not produce a tool error");

    let args = std::fs::read_to_string(stand_in_file(dir.path(), "args.txt")).unwrap();
    let seen: Vec<String> = args.lines().map(str::to_owned).collect();
    assert_eq!(
        seen,
        vec![
            "-T",
            "-o",
            "BatchMode=yes",
            "--",
            "amp2",
            "shep",
            "dial-stdio"
        ],
        "no terminal, no prompts, and the host's own shep on the other end"
    );
}

/// A host that cannot be reached is refused in ssh's own words, which
/// already name it. The failure this pins is wording: the local socket's
/// "no shepherd is running: " prefix doubled onto "no shepherd is running
/// on `amp2`" reads as two complaints about one link.
#[cfg(unix)]
#[tokio::test]
async fn a_host_that_cannot_be_reached_is_refused_in_the_words_that_name_it() {
    let dir = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    let program = dir.path().join("ssh");
    // Exiting with the code a remote `shep dial-stdio` uses when its own
    // shepherd is not running is what makes `SshCause::NoShepherd`, the one
    // cause whose words already say "no shepherd is running".
    std::fs::write(
        &program,
        format!("#!/bin/sh\nexit {}\n", shep_core::exit::DAEMON_UNREACHABLE),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("list_flock must return within the test timeout");
    let refusal = match outcome {
        Ok(_) => panic!("a host that cannot be reached is a refusal, not a flock"),
        Err(refusal) => refusal,
    };

    let structured = refusal
        .structured_content
        .expect("a refusal carries structured content a model can branch on");
    assert_eq!(structured["code"], "no_shepherd", "{structured}");
    let message = structured["message"]
        .as_str()
        .expect("a string")
        .to_string();
    assert!(
        message.contains("amp2"),
        "the refusal names the host: {message}"
    );
    assert_eq!(
        message.matches("no shepherd is running").count(),
        1,
        "the phrase appears once, not once per layer: {message}"
    );
}

/// A host whose own shepherd turns this build's `Hello` down is a version gap,
/// not a host with no shepherd on it. `shep_client::ssh::explain` returns a
/// refusal as it is, so the tool call is the thing that has to say which of the
/// two it is, and a model branching on the code is the reader that acts on it.
#[cfg(unix)]
#[tokio::test]
async fn a_host_that_turns_down_the_handshake_is_refused_as_a_version_gap() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_answering(
        dir.path(),
        Err(shep_core::protocol::RpcError {
            code: shep_core::protocol::RpcErrorCode::ProtocolMismatch,
            message: "this daemon speaks protocol 2".to_string(),
            daemon_version: Some("0.1.8".to_string()),
        }),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("list_flock must return within the test timeout");
    let refusal = match outcome {
        Ok(_) => panic!("a host that refused the handshake answers no flock"),
        Err(refusal) => refusal,
    };

    let structured = refusal
        .structured_content
        .expect("a refusal carries structured content a model can branch on");
    assert_eq!(
        structured["code"], "version_skew",
        "amp2 answered ssh, so a model must not read it as down: {structured}"
    );
    let message = structured["message"]
        .as_str()
        .expect("a string")
        .to_string();
    assert!(message.contains("protocol mismatch"), "{message}");
    assert!(
        !message.contains("no shepherd is running"),
        "and must not read as an absent shepherd: {message}"
    );
}

/// Reads `path` until it holds bytes, or `after` runs out.
///
/// The stand-in writes the request frame it read, and its answer to that
/// request is already on its way out, so a tool call can finish before the
/// file lands.
#[cfg(unix)]
async fn written_within(after: Duration, path: &Path) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + after;
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && !bytes.is_empty()
        {
            return bytes;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("nothing was written to {} within {after:?}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The name-selector rule holds over a host link exactly as it holds over
/// the socket: the request the stand-in received is `SelectorSpec::Name`, so
/// `all`, `/regex/`, `id:` and `fold:` are still not in the grammar a model
/// can reach, and a string `"all"` still means an app called `all`.
///
/// Asserts on the frame the stand-in received, not on the reply it sent.
#[cfg(unix)]
#[tokio::test]
async fn describe_sheep_takes_a_host_and_still_only_ever_builds_a_name_selector() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in(
        dir.path(),
        Response::Described(vec![shep_client::testing::sample_info()]),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.describe_sheep(Parameters(SheepName {
            name: "all".to_string(),
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("describe_sheep must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");
    assert_eq!(result.0.flock.len(), 1);
    assert_eq!(result.0.host.as_deref(), Some("amp2"));

    let bytes = written_within(TEST_TIMEOUT, &stand_in_file(dir.path(), "request-1.bin")).await;
    let envelope: shep_core::protocol::Envelope =
        shep_core::protocol::decode_frame(&bytes).unwrap();
    match envelope.body {
        Request::Describe { selector } => assert_eq!(
            selector,
            SelectorSpec::Name("all".to_string()),
            "a literal name, never `SelectorSpec::All`: {selector:?}"
        ),
        other => panic!("expected Request::Describe, got {other:?}"),
    }
}

/// The bytes a local `list_flock` reply is made of, written down here and
/// not derived from the code: the whole point of the test below is that
/// adding a `host` argument changed none of them.
const LOCAL_LISTING: &str = r#"{"flock":[{"id":1,"name":"web","status":"online","pid":4242,"restarts":3,"uptime_ms":60000,"fold":"backend","depends_on":[],"out_file":"/home/ada/.shep/logs/web-0-out.log","err_file":"/home/ada/.shep/logs/web-0-err.log","cpu_percent":12.5,"memory_bytes":50331648,"cpu_ms":null,"dog":{"kind":"built_in"},"lambs":[{"pid":4243,"name":"node"}],"last_exit":{"code":1,"signal":null},"smit":"▲ main@a1b2c3","instance":null,"handshook":null,"dog_stale":null,"max_memory":null}]}"#;

/// fails if a local call grows a `host` key, or a row's shape moves: the
/// `host` field is `skip_serializing_if`, and this is what that attribute
/// is for.
#[tokio::test]
async fn list_flock_without_a_host_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.list_flock(Parameters(FlockParams { host: None })),
    )
    .await
    .expect("list_flock must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert_eq!(
        serde_json::to_string(&result.0).unwrap(),
        LOCAL_LISTING,
        "a call that names no host answers with the bytes it answered with \
         before a `host` argument existed"
    );

    served.await.expect("the fake daemon task must not panic");
}

/// The clamp both log-reading tools share, at the one place it now
/// lives: absent means 50, a request over the cap lands on 200, a
/// request under it is honored as asked, and a model that asked for no
/// lines at all gets none rather than the default.
#[test]
fn the_tail_clamp_defaults_to_fifty_and_caps_at_two_hundred() {
    assert_eq!(clamp_len(None), 50, "absent means the default");
    assert_eq!(clamp_len(Some(0)), 0, "an asked-for zero stays zero");
    assert_eq!(clamp_len(Some(120)), 120, "a sane request passes through");
    assert_eq!(clamp_len(Some(9999)), 200, "the cap holds");
}

/// An uncapped reply exhausts a model's context; a capped one that does
/// not say so reads as a quiet app.
#[tokio::test]
async fn tail_bleats_caps_its_lines_and_says_when_it_did() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let log_path = dir.path().join("web-out.log");
    let content: String = (1..=4000).map(|n| format!("line-{n}\n")).collect();
    std::fs::write(&log_path, content).unwrap();

    let mut info = shep_client::testing::sample_info();
    info.out_file = Some(log_path.to_string_lossy().into_owned());
    info.err_file = None;

    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Described(vec![info]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: Some(5000),
            host: None,
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert_eq!(
        result.0.out.len(),
        200,
        "the 200 clamp must hold even against a request for 5000: {}",
        result.0.out.len()
    );
    assert!(
        result.0.truncated,
        "hitting the cap must be reported, not silent"
    );
    assert_eq!(
        result.0.out.last().map(String::as_str),
        Some("line-4000"),
        "the tail is the LAST lines, not the first"
    );
    assert!(
        result.0.err.is_empty(),
        "no err_file means an empty tail, not an error"
    );
    // Both files read, so the key is still on the wire and still empty:
    // a model told nothing and a model told nothing is wrong differ.
    assert_eq!(
        serde_json::to_value(&result.0).unwrap()["notes"],
        serde_json::json!([])
    );

    served.await.expect("the fake daemon task must not panic");
}

/// The stand-in's rows and tails for a `tail_bleats` over a host: one row,
/// then the out tail, then the err tail, in the order the tool asks.
#[cfg(unix)]
fn host_tail_replies(out: Vec<String>, out_truncated: bool) -> Vec<Response> {
    use shep_core::protocol::LogTail;

    vec![
        Response::Described(vec![shep_client::testing::sample_info()]),
        Response::LogTail(LogTail::Lines {
            lines: out,
            truncated: out_truncated,
        }),
        Response::LogTail(LogTail::NoPath),
    ]
}

/// A host's files are not files here, so `tail_bleats` over a host asks that
/// shepherd for each stream through `TailLog` and sends the 200 clamp as the
/// line count, whatever the model asked for.
#[cfg(unix)]
#[tokio::test]
async fn tail_bleats_over_a_host_reads_the_tail_through_the_shepherd_and_caps_it() {
    use shep_core::protocol::LogStream;

    let dir = tempfile::tempdir().unwrap();
    // More than the clamp, as a shepherd that ignored the count would send.
    let overlong: Vec<String> = (1..=300).map(|n| format!("line-{n}")).collect();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        host_tail_replies(overlong, false),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: Some(5000),
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(result.0.out.len(), 200, "the clamp holds over a host too");
    assert_eq!(
        result.0.out.last().map(String::as_str),
        Some("line-300"),
        "the tail is the LAST lines"
    );
    assert!(
        result.0.truncated,
        "cutting a longer reply must be reported"
    );
    assert!(result.0.err.is_empty(), "no err path means an empty tail");
    assert!(result.0.notes.is_empty(), "no path is not a missing file");

    let id = shep_client::testing::sample_info().id;
    for (request, stream) in [(2, LogStream::Out), (3, LogStream::Err)] {
        let file = stand_in_file(dir.path(), &format!("request-{request}.bin"));
        let envelope: shep_core::protocol::Envelope =
            shep_core::protocol::decode_frame(&written_within(TEST_TIMEOUT, &file).await).unwrap();
        assert_eq!(
            envelope.body,
            Request::TailLog {
                id,
                stream,
                lines: 200
            }
        );
    }
}

/// A host answers from its own files, so the byte window bounds its reply as
/// it bounds a local read: 200 lines of 2 KiB would be 400 KiB.
#[cfg(unix)]
#[tokio::test]
async fn tail_bleats_over_a_host_keeps_a_reply_inside_the_byte_window() {
    let dir = tempfile::tempdir().unwrap();
    let long = "x".repeat(2047);
    let lines: Vec<String> = (0..200).map(|n| format!("{n:03}{long}")).collect();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        host_tail_replies(lines, false),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: Some(200),
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    // Each line is 2050 bytes with its newline: 127 fit in 256 KiB.
    assert_eq!(result.0.out.len(), 127);
    assert!(
        result.0.out[0].starts_with("073"),
        "the oldest lines go first"
    );
    assert!(result.0.out[126].starts_with("199"));
    assert!(result.0.truncated);
}

/// A shepherd that predates `TailLog` refuses it as `unsupported`; the model
/// is told the fix, as `shep bleats` tells an operator.
#[cfg(unix)]
#[tokio::test]
async fn tail_bleats_over_a_host_that_predates_tail_log_says_to_reload_it() {
    use shep_core::protocol::{RpcError, RpcErrorCode};

    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_results(
        dir.path(),
        Ok(matching_ack()),
        vec![
            Ok(Response::Described(vec![
                shep_client::testing::sample_info(),
            ])),
            Err(RpcError {
                code: RpcErrorCode::Unsupported,
                message: "unknown request".to_string(),
                daemon_version: None,
            }),
        ],
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: None,
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout");
    let Err(refusal) = outcome else {
        panic!("a shepherd that cannot tail is a refusal");
    };

    let structured = refusal.structured_content.expect("structured");
    assert_eq!(structured["code"], "unsupported");
    let message = structured["message"].as_str().expect("a string");
    assert!(message.contains("amp2"), "names the host: {message}");
    assert!(
        message.contains("shep daemon reload"),
        "names the fix: {message}"
    );
}

/// A reply that names its host is how a consumer tells a tail from that
/// machine's logs from this machine's; a local reply keeps its old keys.
#[cfg(unix)]
#[tokio::test]
async fn tail_bleats_over_a_host_names_the_host_in_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        host_tail_replies(vec!["ready".to_string()], false),
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: None,
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(result.0.host.as_deref(), Some("amp2"));
    assert_eq!(result.0.out, ["ready"]);
    assert_eq!(serde_json::to_value(&result.0).unwrap()["host"], "amp2");
}

/// A model reading `out: []` has to be able to tell a quiet sheep from
/// a path that resolves elsewhere under the shepherd than it does here.
#[tokio::test]
async fn tail_bleats_says_which_file_it_could_not_find() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());

    let mut info = shep_client::testing::sample_info();
    info.out_file = Some("logs/out.log".to_string());
    info.err_file = None;

    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Described(vec![info]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: None,
            host: None,
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout")
    .expect("a file that is not there is not a tool error");

    assert!(result.0.out.is_empty());
    assert_eq!(
        result.0.notes.len(),
        1,
        "one note for the one stream with a path: {:?}",
        result.0.notes
    );
    assert!(
        result.0.notes[0].contains("out_file is relative"),
        "the note must say why the shepherd's file is a different one: {:?}",
        result.0.notes
    );
    assert_eq!(
        serde_json::to_value(&result.0).unwrap()["notes"],
        serde_json::json!([result.0.notes[0]]),
        "the note has to reach `structuredContent`, not just the struct"
    );

    served.await.expect("the fake daemon task must not panic");
}

/// `get_metrics` is the whistle's other caller of the host sample, and
/// it crosses the same `spawn_blocking` boundary as the metrics dog's
/// scrape path. Every other tool here has a test; this is the one
/// whose host half was unpinned.
#[tokio::test]
async fn get_metrics_carries_the_host_sample() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert!(
        result.0.host.is_some(),
        "the host sample must survive the trip to the blocking pool"
    );
    assert_eq!(result.0.flock.len(), 1);

    served.await.expect("the fake daemon task must not panic");
}

/// Both handshake fields come off the `HelloAck`, not the `Response::Flock`.
///
/// The sheep's pid is deliberately not the ack's: with both at 4242, a
/// `daemon_pid` taken from the flock would read as the handshake. The version
/// half pins only that the field is filled from the handshake, since
/// `Shepherd::connect` refuses any other version.
#[tokio::test]
async fn get_metrics_reports_the_version_and_pid_from_the_handshake() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let mut sheep = shep_client::testing::sample_info();
    sheep.pid = Some(9999);
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![sheep]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    assert_eq!(
        result.0.flock.first().and_then(|row| row.pid),
        Some(9999),
        "the sheep's own pid is on the same reply, so the two numbers \
         are tellable apart there: {:?}",
        result.0.flock.first().map(|row| row.pid)
    );
    assert_eq!(
        result.0.daemon_pid, 4242,
        "the ack's pid, not the sheep's 9999: {}",
        result.0.daemon_pid
    );
    assert_eq!(
        result.0.daemon_version,
        env!("CARGO_PKG_VERSION"),
        "filled from the handshake, not left at a placeholder: {}",
        result.0.daemon_version
    );

    served.await.expect("the fake daemon task must not panic");
}

/// What a remote shepherd reports for itself. The totals are values no real
/// machine running these tests has, so a reply that sampled this one instead
/// reads as the wrong number.
#[cfg(unix)]
fn remote_usage() -> shep_core::protocol::HostUsage {
    shep_core::protocol::HostUsage {
        cpu_percent: Some(12.5),
        memory_used_bytes: 1_000,
        memory_total_bytes: 2_000,
        disk_bytes_per_second: None,
        network_bytes_per_second: None,
    }
}

/// A `host` makes `get_metrics` ask that host's shepherd for the flock and for
/// its own `HostUsage`, over the one connection, and never sysinfo-sample the
/// machine the model is running on: the totals in the reply are the stand-in's.
#[cfg(unix)]
#[tokio::test]
async fn get_metrics_over_a_host_reads_host_usage_and_never_samples_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        vec![
            Response::Flock(vec![shep_client::testing::sample_info()]),
            Response::HostUsage(Some(remote_usage())),
        ],
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(result.0.host_name.as_deref(), Some("amp2"));
    assert_eq!(result.0.flock.len(), 1);
    let host = result.0.host.expect("the host's usage must be reported");
    assert_eq!(
        (host.memory_total_bytes, host.memory_used_bytes),
        (2_000, 1_000),
        "the stand-in's numbers, not this machine's: {host:?}"
    );

    let bytes = written_within(TEST_TIMEOUT, &stand_in_file(dir.path(), "request-2.bin")).await;
    let envelope: shep_core::protocol::Envelope =
        shep_core::protocol::decode_frame(&bytes).unwrap();
    assert_eq!(envelope.body, Request::HostUsage);
}

/// `HostUsage` carries neither a process count nor an uptime, so a remote
/// reply leaves both keys out instead of reporting this machine's.
#[cfg(unix)]
#[tokio::test]
async fn get_metrics_over_a_host_leaves_processes_and_uptime_out() {
    let dir = tempfile::tempdir().unwrap();
    let program = ssh_stand_in_replying(
        dir.path(),
        Ok(matching_ack()),
        vec![
            Response::Flock(vec![shep_client::testing::sample_info()]),
            Response::HostUsage(Some(remote_usage())),
        ],
    );
    let whistle = whistle_with_hosts(dir.path(), &["amp2"], &program);

    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams {
            host: Some("amp2".to_string()),
        })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a stand-in shepherd must not produce a tool error");

    assert_eq!(
        serde_json::to_value(&result.0).unwrap()["host"],
        serde_json::json!({"memory_total_bytes": 2000, "memory_used_bytes": 1000}),
    );
}

/// A call that names no host answers with the keys it always had: both
/// machine-wide counts present, and no `host_name`.
#[tokio::test]
async fn get_metrics_without_a_host_keeps_its_local_shape() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Flock(vec![shep_client::testing::sample_info()]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.get_metrics(Parameters(MetricsParams { host: None })),
    )
    .await
    .expect("get_metrics must return within the test timeout")
    .expect("a scripted daemon must not produce a tool error");

    let reading = serde_json::to_value(&result.0).unwrap();
    assert!(reading.get("host_name").is_none(), "{reading}");
    let host = &reading["host"];
    assert!(host["processes"].is_u64(), "{host}");
    assert!(host["uptime_seconds"].is_u64(), "{host}");

    served.await.expect("the fake daemon task must not panic");
}

/// A log the shepherd reported but this process cannot read is a
/// different fact from a log that is not there. A note saying "missing"
/// sends a model off to look for a file that exists and is merely
/// unreadable, and an empty `out` with no note reads as a quiet sheep.
#[tokio::test]
async fn tail_bleats_refuses_a_log_it_cannot_read_instead_of_noting_it_missing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = shep_client::testing::control_address(dir.path());

    // A directory where a log file was expected: opening it answers,
    // and the read that follows is what fails.
    let not_a_log = dir.path().join("web-out.log");
    std::fs::create_dir(&not_a_log).unwrap();

    let mut info = shep_client::testing::sample_info();
    info.out_file = Some(not_a_log.to_string_lossy().into_owned());
    info.err_file = None;

    let served = shep_client::testing::serve_one_request(
        &socket,
        matching_ack(),
        Response::Described(vec![info]),
    )
    .await;

    let whistle = whistle_at(socket, dir.path().join("barks.jsonl"));
    let outcome = tokio::time::timeout(
        TEST_TIMEOUT,
        whistle.tail_bleats(Parameters(TailParams {
            name: "web".to_string(),
            lines: None,
            host: None,
        })),
    )
    .await
    .expect("tail_bleats must return within the test timeout");
    // `Json<T>` carries no `Debug`, so this cannot be `expect_err`.
    let refusal = match outcome {
        Ok(_) => panic!("a log that cannot be read is a refusal, not an empty tail"),
        Err(refusal) => refusal,
    };

    assert_eq!(refusal.is_error, Some(true));
    let structured = refusal
        .structured_content
        .expect("a refusal carries structured content a model can branch on");
    assert_eq!(
        structured["code"], "log_unreadable",
        "a distinct code from `internal`, and not a missing-file note: {structured}"
    );
    let message = structured["message"].as_str().expect("a string");
    assert!(
        message.contains(&not_a_log.to_string_lossy().to_string()),
        "the message names the path this process tried: {message}"
    );

    served.await.expect("the fake daemon task must not panic");
}
