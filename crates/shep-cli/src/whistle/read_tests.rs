//! `read`'s tests: the five read tools, the local socket, and a host named
//! out of `shep.toml`'s `hosts` list.
//!
//! Split out of `read.rs` under IR-48. Reached as `read::tests` through the
//! `#[path]` declaration at the end of that file, so every test keeps the
//! path a caller cites it by.

use std::time::Duration;

use shep_core::paths::ShepPaths;
use shep_core::protocol::{DogSource, ProcessInfo};
use shep_core::status::ProcStatus;

use super::*;
use crate::whistle::gate;
use crate::whistle::matching_ack;

/// How long a test waits for a tool call before treating it as hung.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A [`shep_core::protocol::HelloAck`] whose version matches this
/// binary, since `sample_ack`'s fixed `"9.9.9"` would be refused by
/// the guard in `Shepherd::call_with_ack`.
/// A `ShepPaths` naming only the three fields any test here reads: the
/// socket, the barks file and the `shep.toml` the hosts list is read from.
/// The rest carry an empty placeholder.
fn test_paths(
    socket: std::path::PathBuf,
    barks: std::path::PathBuf,
    daemon_config: std::path::PathBuf,
) -> ShepPaths {
    ShepPaths {
        home: std::path::PathBuf::new(),
        daemon_config,
        dogs_config: std::path::PathBuf::new(),
        snapshot: std::path::PathBuf::new(),
        logs: std::path::PathBuf::new(),
        pids: std::path::PathBuf::new(),
        run: std::path::PathBuf::new(),
        socket,
        barks,
        kv: std::path::PathBuf::new(),
        overrides: std::path::PathBuf::new(),
        secrets: std::path::PathBuf::new(),
        secrets_cache: std::path::PathBuf::new(),
    }
}

fn whistle_at(socket: std::path::PathBuf, barks_path: std::path::PathBuf) -> Whistle {
    Whistle::new(
        test_paths(socket, barks_path, dir_placeholder("no-shep.toml")),
        gate::Control::ReadOnly,
    )
}

/// A path nothing is ever written to, for the `daemon_config` of a
/// `Whistle` whose test wants no hosts listed at all.
fn dir_placeholder(name: &str) -> std::path::PathBuf {
    std::path::Path::new("/nonexistent").join(name)
}

/// Writes a `shep.toml` into `dir` listing `hosts`, and a `Whistle` at it
/// whose host calls run `program` in place of `ssh`.
///
/// The list is written to the file rather than handed to the `Whistle`, so
/// these tests read the same `hosts` array `shep host add` writes.
#[cfg(unix)]
fn whistle_with_hosts(dir: &Path, hosts: &[&str], program: &Path) -> Whistle {
    let quoted: Vec<String> = hosts.iter().map(|host| format!("\"{host}\"")).collect();
    let config = dir.join("shep.toml");
    std::fs::write(&config, format!("hosts = [{}]\n", quoted.join(", "))).unwrap();
    Whistle::for_test_with_ssh_program(
        test_paths(
            shep_client::testing::control_address(dir),
            dir.join("barks.jsonl"),
            config,
        ),
        gate::Control::ReadOnly,
        program,
    )
}

/// One value as the length-prefixed frame a shepherd would put on the wire.
#[cfg(unix)]
fn framed<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let payload = shep_core::protocol::encode_frame(value).unwrap();
    let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
    bytes.extend_from_slice(&payload);
    bytes
}

/// The three files a stand-in ssh writes, named the way the script names
/// them.
#[cfg(unix)]
fn stand_in_file(dir: &Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("ssh-{name}"))
}

/// Writes an executable stand-in for `ssh` into `dir` and returns its path.
///
/// It records the arguments it was run with, waits for the one request the
/// client sends, writes that frame to disk for a test that wants the request
/// back, and answers it with `response`. Answering only once the request has
/// arrived is what the fixture is for: `shep_client::actor`'s loop picks
/// between a command and an incoming frame, so a reply already sitting in the
/// pipe when the request is sent can be read before anything is waiting for
/// it, and dropped.
///
/// The request is read as the daemon's codec frames it, twice: the
/// client's `Hello` first, then the one request. The ack goes out between
/// them, so the canned answer cannot sit in the pipe waiting.
///
/// Unix only: the stand-in is a `/bin/sh` script, the same shape
/// `crates/shep-cli/tests/cli_e2e/ssh_stand_in.rs` uses.
#[cfg(unix)]
fn ssh_stand_in(dir: &Path, response: Response) -> std::path::PathBuf {
    use shep_core::protocol::{HelloReply, Reply};
    use std::os::unix::fs::PermissionsExt as _;

    // The ack has to carry this binary's own version: `Shepherd::connect`
    // refuses a skew, and `matching_ack`'s local-socket version is the one
    // a host link is held to as well.
    let ack = matching_ack();
    // `shep_client::actor`'s `next_id` starts at 1, and every test here
    // sends one request.
    let reply = Reply {
        id: 1,
        result: Ok(response),
    };
    let handshake: HelloReply = Ok(ack);
    std::fs::write(stand_in_file(dir, "ack.bin"), framed(&handshake)).unwrap();
    std::fs::write(stand_in_file(dir, "reply.bin"), framed(&reply)).unwrap();

    let script = format!(
        "#!/bin/sh\n\
         read_frame() {{ set -- $(dd bs=1 count=4 2>/dev/null | od -An -tu1); \
         dd bs=1 count=$(( $1 * 16777216 + $2 * 65536 + $3 * 256 + $4 )) 2>/dev/null; }}\n\
         printf '%s\\n' \"$@\" > '{}'\n\
         read_frame > /dev/null\n\
         cat '{}'\n\
         read_frame > '{}'\n\
         cat '{}'\n",
        stand_in_file(dir, "args.txt").display(),
        stand_in_file(dir, "ack.bin").display(),
        stand_in_file(dir, "request.bin").display(),
        stand_in_file(dir, "reply.bin").display(),
    );
    let program = dir.join("ssh");
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

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

    let bytes = written_within(TEST_TIMEOUT, &stand_in_file(dir.path(), "request.bin")).await;
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
    let result = tokio::time::timeout(TEST_TIMEOUT, whistle.get_metrics())
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

/// The two handshake fields are the only place a model is told which
/// shepherd it is looking at, and both come off the `HelloAck` the same
/// call returns, not off the `Response::Flock`.
///
/// The sheep's own pid is deliberately not the ack's: with both at
/// 4242, `daemon_pid` taken from the flock instead of the ack would
/// read as the handshake. The version half cannot be separated that
/// way: `Shepherd::connect` refuses any ack whose version is not this
/// binary's (`VersionGuard::Enforce`'s arm in `crate::version_guard`),
/// so what this pins for the version is that the field is filled from
/// the handshake rather than left at a placeholder, which is what a
/// `call` that dropped the ack would get wrong.
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
    let result = tokio::time::timeout(TEST_TIMEOUT, whistle.get_metrics())
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
