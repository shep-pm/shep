//! The ask, against a scripted shepherd: what a dog's refusal hands back,
//! what the trigger carries, and the three cases with nobody to ask.

use std::fmt::Write as _;
use std::path::Path;

use crate::commands::shep_toml::ShepToml;
use shep_client::testing::{control_address, fake_client_answering_refusals};
use shep_client::{Client, RequestError};
use shep_core::dogs::validate::VALIDATE_ACTION;
use shep_core::paths::ShepPaths;
use shep_core::protocol::{
    ActionOutcome, ActionReply, Envelope, Request, Response, RpcError, RpcErrorCode, SelectorSpec,
};

use super::*;

/// The dog `kelpie`, adopted at `binary`, with `manifest` cached for it.
///
/// The cache is written by hand rather than by probing a binary: this is the
/// half that decides whether to ask, and a cached manifest is exactly what
/// `shep set` reads on a dog `shep adopt` already read.
pub(crate) fn home_with(dir: &Path, manifest: Option<&str>) -> ShepPaths {
    let paths = ShepPaths::resolve(&|_| None, dir);
    std::fs::create_dir_all(&paths.home).unwrap();
    let binary = dir.join("shep-kelpie");
    std::fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
    // `None` is a dog this machine never adopted, which is the built-in
    // case. An adopted one is recorded by the writer `shep adopt` uses, so
    // the path is quoted the way TOML quotes a path. A windows runner
    // spells temp paths with backslashes, and TOML reads one as an escape.
    let Some(manifest) = manifest else {
        return paths;
    };
    ShepToml::edit(&paths.daemon_config, |doc| {
        doc.adopt_dog("kelpie", &binary, true).unwrap();
    })
    .unwrap();
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    context.update(&std::fs::read(&binary).unwrap());
    let digest = context.finish();
    let hex = digest.as_ref().iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    let cache = paths.home.join("manifests");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(format!("{hex}.json")), manifest).unwrap();
    paths
}

/// A manifest declaring `validate`, which is the only manifest that gets asked.
pub(crate) fn validating() -> &'static str {
    r#"{"manifest":1,"name":"kelpie","version":"0.2.0","protocol":12,"channel":true,"validate":true}"#
}

/// The same manifest with `validate` left out, which is what every dog adopted
/// before this field did anything has cached.
fn declares_nothing() -> &'static str {
    r#"{"manifest":1,"name":"kelpie","version":"0.2.0","protocol":12,"channel":true}"#
}

fn table(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

fn trigger_reply(outcome: ActionOutcome) -> impl Fn(&Request) -> Result<Response, RpcError> {
    move |request| match request {
        Request::Trigger { .. } => Ok(Response::Triggered(vec![ActionReply {
            id: 7,
            name: "kelpie".to_string(),
            outcome: outcome.clone(),
        }])),
        _other => Ok(Response::Pong),
    }
}

/// A daemon that answers the ask with `outcome`, and the envelopes it saw.
async fn asked(
    dir: &Path,
    manifest: Option<&str>,
    outcome: ActionOutcome,
) -> (
    Client,
    ShepPaths,
    tokio::sync::mpsc::UnboundedReceiver<Envelope>,
) {
    let paths = home_with(dir, manifest);
    let path = control_address(dir);
    let (client, envelopes) = fake_client_answering_refusals(&path, trigger_reply(outcome)).await;
    (client, paths, envelopes)
}

/// The refusal, in the dog's own words, is what the caller reports.
#[tokio::test]
async fn a_dog_that_refuses_hands_its_reason_to_the_operator() {
    let dir = tempfile::tempdir().unwrap();
    let (client, paths, _envelopes) = asked(
        dir.path(),
        Some(validating()),
        ActionOutcome::Replied {
            body: r#"{"accept":false,"reason":"this checkout belongs to another project"}"#
                .to_string(),
        },
    )
    .await;

    let failure = ask_dog(
        &client,
        &paths,
        "kelpie",
        &table(&[("checkout", serde_json::json!("/other"))]),
        None,
    )
    .await
    .expect_err("a refusal is a refusal");

    assert_eq!(
        failure,
        AskFailure::Refused {
            reason: "this checkout belongs to another project".to_string()
        }
    );
    assert_eq!(
        failure.request_error(),
        RequestError::Rpc(RpcError {
            code: RpcErrorCode::InvalidConfig,
            message: "this checkout belongs to another project".to_string(),
            daemon_version: None,
        }),
        "the dashboard reports a dog's refusal as a refused write"
    );
}

/// The dog is asked about the table the write would leave behind, by name,
/// over the one action shep defines.
#[tokio::test]
async fn the_ask_carries_the_table_the_write_would_put_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let (client, paths, mut envelopes) = asked(
        dir.path(),
        Some(validating()),
        ActionOutcome::Replied {
            body: r#"{"accept":true}"#.to_string(),
        },
    )
    .await;

    ask_dog(
        &client,
        &paths,
        "kelpie",
        &table(&[("allowed_domains", serde_json::json!(["example.com"]))]),
        Some("api"),
    )
    .await
    .expect("the dog accepted");

    let envelope = envelopes.recv().await.expect("the ask went out");
    let Request::Trigger {
        selector,
        action,
        params,
    } = envelope.body
    else {
        panic!("a dog is asked over a trigger: {:?}", envelope.body);
    };
    assert_eq!(selector, SelectorSpec::Name("kelpie".to_string()));
    assert_eq!(action, VALIDATE_ACTION);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&params.expect("a table is sent")).unwrap(),
        serde_json::json!({
            "table": { "allowed_domains": ["example.com"] },
            "sheep": "api",
        })
    );
}

/// Silence is not acceptance: the dog that was asked and answered nothing has
/// not cleared the write.
#[tokio::test]
async fn a_dog_that_answers_nothing_in_time_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let (client, paths, _envelopes) =
        asked(dir.path(), Some(validating()), ActionOutcome::TimedOut).await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("a timeout is a refusal");

    let AskFailure::Refused { reason } = failure else {
        panic!("a timeout is a refusal: {failure:?}");
    };
    assert!(reason.contains("`kelpie`"), "{reason}");
    assert!(reason.contains("action_timeout"), "{reason}");
}

/// An answer that is not a verdict has not accepted the write either.
#[tokio::test]
async fn a_dog_that_answers_something_else_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let (client, paths, _envelopes) = asked(
        dir.path(),
        Some(validating()),
        ActionOutcome::Replied {
            body: "ok".to_string(),
        },
    )
    .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("an answer that is not a verdict is not an acceptance");

    let AskFailure::Refused { reason } = failure else {
        panic!("{failure:?}");
    };
    assert!(reason.contains("cannot read"), "{reason}");
    assert!(reason.contains("ok"), "{reason}");
}

/// The acceptance criterion in its plainest form: a dog that accepts lets the
/// write through.
#[tokio::test]
async fn a_dog_that_accepts_clears_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let (client, paths, _envelopes) = asked(
        dir.path(),
        Some(validating()),
        ActionOutcome::Replied {
            body: r#"{"accept":true}"#.to_string(),
        },
    )
    .await;

    assert_eq!(
        ask_dog(
            &client,
            &paths,
            "kelpie",
            &table(&[("keep", serde_json::json!(2))]),
            None,
        )
        .await,
        Ok(())
    );
}

/// A dog that declared nothing is never asked, and never costs a round trip.
#[tokio::test]
async fn a_dog_without_validate_is_never_asked() {
    for manifest in [None, Some(declares_nothing())] {
        let dir = tempfile::tempdir().unwrap();
        let (client, paths, mut envelopes) =
            asked(dir.path(), manifest, ActionOutcome::TimedOut).await;

        assert_eq!(
            ask_dog(&client, &paths, "kelpie", &table(&[]), None).await,
            Ok(()),
            "{manifest:?}"
        );
        assert_eq!(
            envelopes.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty),
            "{manifest:?}: nothing was sent to the dog"
        );
    }
}

/// A dog an operator has not started yet is the ordinary case for a first
/// write, and nothing in the flock can refuse it.
#[tokio::test]
async fn a_dog_that_is_not_in_the_flock_is_not_asked() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    let path = control_address(dir.path());
    let (client, _envelopes) = fake_client_answering_refusals(&path, |request| match request {
        Request::Trigger { .. } => Err(RpcError {
            code: RpcErrorCode::NotFound,
            message: "no sheep named kelpie".to_string(),
            daemon_version: None,
        }),
        _other => Ok(Response::Pong),
    })
    .await;

    assert_eq!(
        ask_dog(&client, &paths, "kelpie", &table(&[]), None).await,
        Ok(())
    );
}

/// A dog registered but not running, or one being swapped out, has no live
/// channel to answer on, and a write is what makes it worth starting.
#[tokio::test]
async fn a_dog_with_no_live_channel_is_not_asked() {
    for outcome in [
        ActionOutcome::DogNoChannel,
        ActionOutcome::NoChannel,
        ActionOutcome::Skipped,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (client, paths, _envelopes) =
            asked(dir.path(), Some(validating()), outcome.clone()).await;

        assert_eq!(
            ask_dog(&client, &paths, "kelpie", &table(&[]), None).await,
            Ok(()),
            "{outcome:?}"
        );
    }
}

/// An ask that never reached the shepherd is not the dog refusing.
#[tokio::test]
async fn an_ask_the_shepherd_refused_is_not_the_dog_refusing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    let path = control_address(dir.path());
    let (client, _envelopes) = fake_client_answering_refusals(&path, |request| match request {
        Request::Trigger { .. } => Err(RpcError {
            code: RpcErrorCode::Forbidden,
            message: "dog `kelpie` was refused `trigger`".to_string(),
            daemon_version: None,
        }),
        _other => Ok(Response::Pong),
    })
    .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("the ask failed");

    assert!(matches!(failure, AskFailure::Transport(_)), "{failure:?}");
}

/// The manifest lookup survives the separator the platform spells a path with.
///
/// A windows runner hands every temp path with backslashes, and a raw backslash
/// inside a TOML basic string is an escape: `\s` is a syntax error and `\b` is
/// a backspace. A backslash is a legal file name character on unix, so this
/// machine can reach the same shape.
#[tokio::test]
async fn an_adopted_path_with_a_backslash_in_it_is_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("shep\\home");
    std::fs::create_dir_all(&home).unwrap();
    let (client, paths, mut envelopes) = asked(
        &home,
        Some(validating()),
        ActionOutcome::Replied {
            body: r#"{"accept":true}"#.to_string(),
        },
    )
    .await;

    assert_eq!(
        ShepToml::read_only(&paths.daemon_config)
            .expect("the fixture's shep.toml parses")
            .adopted_dog_path("kelpie")
            .expect("the adopted path reads back"),
        home.join("shep-kelpie"),
        "TOML quotes a path: a raw backslash in a basic string is an escape sequence"
    );
    assert_eq!(
        ask_dog(&client, &paths, "kelpie", &table(&[]), None).await,
        Ok(()),
        "the cache is keyed off the adopted binary, so the adopted path has to read back"
    );
    assert!(
        envelopes.try_recv().is_ok(),
        "a dog that declared validate is asked even with a backslash in its path"
    );
}

/// A cache file that will not parse is not a manifest that declares nothing.
///
/// The distinction is the finding this test pins: a dog that declared
/// `validate` and whose manifest shep cannot read is a dog whose check the
/// operator believes is running, and reading it as "nothing declared" would
/// let the write through with the check never run.
#[tokio::test]
async fn a_manifest_shep_cannot_read_refuses_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    let cache = paths.home.join("manifests");
    let file = std::fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .next()
        .expect("the fixture cached one manifest");
    std::fs::write(&file, "{\"manifest\":1,\"name\":").unwrap();

    let (client, _envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("an unreadable manifest is not an acceptance");
    let AskFailure::Unknown { reason } = failure else {
        panic!("an unreadable manifest is no verdict, and no dog's refusal either: {failure:?}");
    };
    assert!(
        reason.contains("kelpie") && reason.contains("manifest") && reason.contains("consulted"),
        "the sentence names the dog, what could not be read, and what that blocks: {reason}"
    );
}

/// A `shep.toml` that will not parse leaves `validate` unknown too.
#[tokio::test]
async fn a_shep_toml_that_will_not_parse_refuses_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    std::fs::write(&paths.daemon_config, "[daemon\nenabled_dogs = ").unwrap();

    let (client, _envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("a config shep cannot read is not a config that says nothing");
    assert!(
        matches!(&failure, AskFailure::Unknown { reason } if reason.contains("kelpie")),
        "{failure:?}"
    );
}

/// A shepherd that matched the dog and answered with no row answered no verdict.
#[tokio::test]
async fn an_answer_with_no_row_for_the_dog_refuses_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    let (client, _envelopes) = shep_client::testing::fake_client_answering_refusals(
        &control_address(dir.path()),
        |request| match request {
            Request::Trigger { .. } => Ok(Response::Triggered(Vec::new())),
            _other => Ok(Response::Pong),
        },
    )
    .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("no row is no verdict");
    assert!(
        matches!(&failure, AskFailure::Unknown { reason } if reason.contains("no row")),
        "{failure:?}"
    );
}

/// An adopted dog with no cached manifest is read the way `shep adopt` reads
/// it, and a dog that answers no manifest cannot have declared `validate`.
#[cfg(unix)]
#[tokio::test]
async fn a_dog_that_answers_no_manifest_at_all_is_not_asked() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let binary = dir.path().join("shep-kelpie");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    ShepToml::edit(&paths.daemon_config, |doc| {
        doc.adopt_dog("kelpie", &binary, true).unwrap();
    })
    .unwrap();

    let (client, mut envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    assert_eq!(
        ask_dog(&client, &paths, "kelpie", &table(&[]), None).await,
        Ok(())
    );
    assert_eq!(
        envelopes.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty),
        "a dog that answers no manifest has nowhere to declare validate"
    );
}

/// The adopted binary `paths` records for `kelpie`.
///
/// `#[cfg(unix)]` with its only caller: the probe it exists to replace runs a
/// shell script.
#[cfg(unix)]
fn adopted_binary(paths: &ShepPaths) -> std::path::PathBuf {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .adopted_dog_path("kelpie")
        .expect("the fixture adopted kelpie")
}

/// Rewrites `kelpie`'s adopted binary to run `body` when asked for its manifest,
/// and makes it one the kernel will run. The new bytes change the digest the
/// cache is keyed on, so the cached manifest is missed and the probe is what
/// answers.
#[cfg(unix)]
fn replace_the_dog_with_a_probe(paths: &ShepPaths, body: &str) {
    let binary = adopted_binary(paths);
    std::fs::write(&binary, format!("#!/bin/sh\n{body}\nexit 0\n")).unwrap();
    make_executable_at(&binary);
}

/// Makes the fixture binary one the probe can run, without touching its bytes,
/// so the digest the cache is keyed on still names it.
#[cfg(unix)]
fn make_executable(paths: &ShepPaths) {
    make_executable_at(&adopted_binary(paths));
}

#[cfg(unix)]
fn make_executable_at(binary: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every request the run put on the wire.
///
/// `#[cfg(unix)]` with its only callers: the dogs it counts are shell scripts.
#[cfg(unix)]
fn sent(mut envelopes: tokio::sync::mpsc::UnboundedReceiver<Envelope>) -> Vec<Request> {
    let mut out = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        out.push(envelope.body);
    }
    out
}

/// A dog that cannot have declared `validate` is never asked, and the write
/// lands. `--shep-manifest` is newer than most dogs: one built on
/// `shep_client`'s argument parser rejects the unknown flag with a non-zero
/// exit, and a dog that ignores it starts its main loop and is killed when the
/// probe budget runs out. Neither ever printed a manifest, so neither has a
/// declaration to lose, and a gate that refused them would make every dog
/// adopted before the flag unconfigurable.
#[cfg(unix)]
#[tokio::test]
async fn a_dog_that_rejects_the_manifest_flag_is_never_asked() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    replace_the_dog_with_a_probe(
        &paths,
        r#"if [ "$1" = --shep-manifest ]; then
  echo 'unknown flag: --shep-manifest' >&2
  exit 1
fi"#,
    );

    let (client, envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let landed = ask_dog(&client, &paths, "kelpie", &table(&[]), None).await;
    assert!(landed.is_ok(), "{landed:?}");
    let requests = sent(envelopes);
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::Trigger { .. })),
        "a dog that never printed a manifest is no dog to ask: {requests:?}"
    );
}

/// The same for a dog whose probe was still running when the budget ran out.
#[cfg(unix)]
#[tokio::test]
async fn a_dog_that_never_answers_the_manifest_flag_is_never_asked() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    replace_the_dog_with_a_probe(
        &paths,
        r#"if [ "$1" = --shep-manifest ]; then
  sleep 3
fi"#,
    );

    let (client, envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let landed = ask_dog(&client, &paths, "kelpie", &table(&[]), None).await;
    assert!(landed.is_ok(), "{landed:?}");
    let requests = sent(envelopes);
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::Trigger { .. })),
        "a probe that ran out of budget said nothing about what the dog wants: {requests:?}"
    );
}

/// And for one that answers the flag with a usage banner on stdout: what it
/// printed is not a manifest, which is the same answer as printing nothing.
#[cfg(unix)]
#[tokio::test]
async fn a_dog_that_prints_no_manifest_for_the_flag_is_never_asked() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some(validating()));
    replace_the_dog_with_a_probe(&paths, "echo 'not a manifest'");

    let (client, envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let landed = ask_dog(&client, &paths, "kelpie", &table(&[]), None).await;
    assert!(landed.is_ok(), "{landed:?}");
    assert!(
        !sent(envelopes)
            .iter()
            .any(|request| matches!(request, Request::Trigger { .. })),
        "no manifest, no declaration, no ask"
    );
}

/// A cache file filed under this binary's own digest is a manifest shep printed
/// for it once. That record is what a corrupt one loses, and losing it is not
/// the same as never having one: the dog asked to be consulted, and this read
/// cannot tell what it asked.
#[cfg(unix)]
#[tokio::test]
async fn a_cached_manifest_that_neither_reads_nor_answers_refuses_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let paths = home_with(dir.path(), Some("{ truncated"));
    make_executable(&paths);

    let (client, _envelopes) =
        shep_client::testing::fake_client_answering_refusals(&control_address(dir.path()), |_| {
            Ok(Response::Pong)
        })
        .await;

    let failure = ask_dog(&client, &paths, "kelpie", &table(&[]), None)
        .await
        .expect_err("a record of a manifest that cannot be read is no acceptance");
    let AskFailure::Unknown { reason } = failure else {
        panic!("a lost record is no dog's refusal: {failure:?}");
    };
    assert!(
        reason.contains("cannot be read") && reason.contains("answered nothing"),
        "{reason}"
    );
}
