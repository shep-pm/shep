use super::*;

/// fails if a restart that the shepherd refused an app of exits 0 with
/// that app's row simply absent, which is what shipped between #166 and
/// this: `shep restart all` is a deploy step, and exit 0 there says the
/// whole fold came back when part of it did not.
#[tokio::test]
async fn a_restart_the_shepherd_refused_an_app_of_names_it_and_exits_non_zero() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Restart { .. } => Response::Restarted {
            accepted: vec![reloaded_api()],
            refused: vec![SheepRefusal::new(
                "db",
                "selector matched no registered sheep",
            )],
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = restart_against(&client, "all").await;

    assert_eq!(
        code,
        ExitCode::Failure,
        "a fold restarted around a refused app is not a success: {said}"
    );
    assert!(
        said.contains("did not restart")
            && said.contains("db")
            && said.contains("no registered sheep"),
        "the refused app and the shepherd's reason reach the operator: {said}"
    );
    assert!(
        printed.contains("api"),
        "and what did restart is still printed: {printed}"
    );
}

/// fails if an ordinary restart starts exiting non-zero: the refused list
/// is empty on every restart nothing was refused of, which is all of them
/// bar the staged walk.
#[tokio::test]
async fn a_restart_that_refused_nothing_still_exits_zero() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Restart { .. } => Response::Restarted {
            accepted: vec![reloaded_api()],
            refused: Vec::new(),
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = restart_against(&client, "all").await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(printed.contains("api"), "{printed}");
    assert!(said.is_empty(), "nothing to say: {said}");
}

/// fails if a partially-refused restart prints two top-level JSON
/// objects for one invocation, the guard `reload` already carries: a
/// consumer piping the run through `jq` either takes a parse error or
/// reads the first object and believes the whole fold came back.
#[tokio::test]
async fn a_partly_refused_restart_prints_one_json_envelope_carrying_both_halves() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Restart { .. } => Response::Restarted {
            accepted: vec![reloaded_api()],
            refused: vec![SheepRefusal::new(
                "db",
                "selector matched no registered sheep",
            )],
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = restart_against_in(&client, "all", Format::Json).await;

    assert_eq!(
        code,
        ExitCode::Failure,
        "the exit code is unchanged: {said}"
    );
    assert_eq!(
        objects_in(&printed),
        1,
        "one envelope per invocation, refusal or no refusal: {printed}"
    );
    assert!(
        said.is_empty(),
        "and no second object beside it on stderr: {said}"
    );

    let envelope: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(envelope["command"], "restart", "{envelope}");
    assert_eq!(
        envelope["schema_version"], 1,
        "a key added beside `data` is additive: {envelope}"
    );
    assert_eq!(
        envelope["data"][0]["name"], "api",
        "what restarted is still the answer: {envelope}"
    );
    assert_eq!(
        envelope["refused"][0]["name"], "db",
        "and what did not is named in the same object: {envelope}"
    );
    assert_eq!(
        envelope["refused"][0]["reason"], "selector matched no registered sheep",
        "with the shepherd's own reason: {envelope}"
    );
}

/// fails if an ordinary restart's envelope grows a key: `refused` is
/// carried only by a run that had something to refuse, so every existing
/// consumer reads the same three fields it always did and
/// `SCHEMA_VERSION` stays 1.
#[tokio::test]
async fn a_restart_that_refused_nothing_carries_no_refused_key() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Restart { .. } => Response::Restarted {
            accepted: vec![reloaded_api()],
            refused: Vec::new(),
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = restart_against_in(&client, "all", Format::Json).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    let envelope: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert!(
        envelope.get("refused").is_none(),
        "nothing was refused, so nothing says so: {envelope}"
    );
}

/// fails if a respawn that could not exec takes the refused apps down
/// with it. Both failures land in one invocation: `api` came back
/// `errored`, so stdout stays empty and the `--format json` envelope
/// that would have carried `db` was never printed. One error object is
/// all `cli.rs` publishes, so `db` rides that object rather than being
/// dropped.
#[tokio::test]
async fn a_restart_that_both_failed_to_spawn_and_refused_an_app_names_both() {
    use shep_client::testing::fake_client_answering;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Restart { .. } => Response::Restarted {
            accepted: vec![ProcessInfo::builder(1, "api", ProcStatus::Errored).build()],
            refused: vec![SheepRefusal::new(
                "db",
                "selector matched no registered sheep",
            )],
        },
        Request::ListFlock => Response::Flock(Vec::new()),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = restart_against(&client, "all").await;

    assert_eq!(
        code,
        ExitCode::SpawnFailed,
        "a child that could not exec is the louder failure: {said}"
    );
    assert!(
        said.contains("api") && said.contains("did not come back up"),
        "the sheep that could not spawn is named: {said}"
    );
    assert!(
        said.contains("db") && said.contains("did not restart"),
        "and so is the one the walk went around: {said}"
    );
    assert!(
        printed.is_empty(),
        "a failed verb prints no table: {printed}"
    );
}

/// fails if a reload that the shepherd refused an app of exits 0 with
/// that app's row simply absent: `shep reload all` is a deploy step, and
/// exit 0 there says the whole fold reloaded when part of it did not.
#[tokio::test]
async fn a_reload_the_shepherd_refused_an_app_of_names_it_and_exits_non_zero() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Reload { .. } => Response::Reloading {
            accepted: vec![reloaded_api()],
            refused: vec![SheepRefusal::new("db", "db is already being reloaded")],
        },
        // The table a lifecycle verb prints is a fresh listing, not the
        // reply's own rows.
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reload_against(&client, "all").await;

    assert_eq!(
        code,
        ExitCode::Failure,
        "a fold reloaded around a refused app is not a success: {said}"
    );
    assert!(
        said.contains("db") && said.contains("already being reloaded"),
        "the refused app and the shepherd's reason reach the operator: {said}"
    );
    assert!(
        printed.contains("api"),
        "and what did reload is still printed: {printed}"
    );
}

/// fails if an ordinary reload starts exiting non-zero: the refused list
/// is empty on every reload nothing was refused of, which is all of them
/// bar the staged walk.
#[tokio::test]
async fn a_reload_that_refused_nothing_still_exits_zero() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Reload { .. } => Response::Reloading {
            accepted: vec![reloaded_api()],
            refused: Vec::new(),
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reload_against(&client, "all").await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert!(printed.contains("api"), "{printed}");
    assert!(said.is_empty(), "nothing to say: {said}");
}

/// fails if a partially-refused reload prints two top-level JSON
/// objects for one invocation: `cli.rs` publishes `--format json` as
/// one object per invocation, and a consumer piping the run through
/// `jq` either takes a parse error or reads the first object and
/// believes the whole fold reloaded.
#[tokio::test]
async fn a_partly_refused_reload_prints_one_json_envelope_carrying_both_halves() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Reload { .. } => Response::Reloading {
            accepted: vec![reloaded_api()],
            refused: vec![SheepRefusal::new("db", "db is already being reloaded")],
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reload_against_in(&client, "all", Format::Json).await;

    assert_eq!(
        code,
        ExitCode::Failure,
        "the exit code is unchanged: {said}"
    );
    assert_eq!(
        objects_in(&printed),
        1,
        "one envelope per invocation, refusal or no refusal: {printed}"
    );
    assert!(
        said.is_empty(),
        "and no second object beside it on stderr: {said}"
    );

    let envelope: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(envelope["command"], "reload", "{envelope}");
    assert_eq!(
        envelope["schema_version"], 1,
        "a key added beside `data` is additive: {envelope}"
    );
    assert_eq!(
        envelope["data"][0]["name"], "api",
        "what reloaded is still the answer: {envelope}"
    );
    assert_eq!(
        envelope["refused"][0]["name"], "db",
        "and what did not is named in the same object: {envelope}"
    );
    assert_eq!(
        envelope["refused"][0]["reason"], "db is already being reloaded",
        "with the shepherd's own reason: {envelope}"
    );
}

/// fails if a JSON walk whose envelope never printed drops the refused
/// app's name with it
///
/// The other side of `carried` is
/// [`a_partly_refused_reload_prints_one_json_envelope_carrying_both_halves`].
///
/// Here no envelope prints, so stderr is the only stream left to name `db`.
#[tokio::test]
async fn a_json_walk_whose_envelope_never_printed_names_the_refused_apps_on_stderr() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Reload {
            selector: SelectorSpec::Name(name),
        } if name == "db" => Response::Reloading {
            accepted: Vec::new(),
            refused: vec![SheepRefusal::new("db", "db is already being reloaded")],
        },
        _ => Response::Pong,
    })
    .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Json);
        reload(
            &client,
            &mut streams,
            &SelectorArgs {
                selectors: vec!["api".to_string(), "db".to_string()],
            },
        )
        .await
    };
    let printed = String::from_utf8(out).unwrap();
    let said = String::from_utf8(err).unwrap();

    assert_eq!(
        code,
        ExitCode::Internal,
        "the unreadable response keeps its own class, as in a load: {said}"
    );
    assert!(
        printed.is_empty(),
        "no envelope printed, so none can be read: {printed}"
    );
    assert!(
        said.contains("db") && said.contains("already being reloaded"),
        "stderr is the only stream left, and it names the refused app: {said}"
    );
}

/// fails if an ordinary reload's envelope grows a key: `refused` is
/// carried only by a run that had something to refuse, so every
/// existing consumer reads the same three fields it always did.
#[tokio::test]
async fn a_reload_that_refused_nothing_carries_no_refused_key() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::Reload { .. } => Response::Reloading {
            accepted: vec![reloaded_api()],
            refused: Vec::new(),
        },
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reload_against_in(&client, "all", Format::Json).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    let envelope: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert!(
        envelope.get("refused").is_none(),
        "nothing was refused, so nothing says so: {envelope}"
    );
}

/// A load whose answer this client cannot read means the whole file went
/// nowhere, and a daemon-side fault takes its own class's code rather
/// than the refusal's.
#[tokio::test]
async fn a_load_that_failed_for_another_reason_exits_with_its_own_class() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = write_zam_flockfile(dir.path());
    let path = shep_client::testing::control_address(dir.path());
    let flock = a_clustered_flock(&[0, 1, 2]);
    // `Pong` to an `ApplyConfig`. `Response` is `#[non_exhaustive]`, so
    // this is also the shape a newer daemon produces.
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::ListFlock => Response::Flock(flock.clone()),
        _ => Response::Pong,
    })
    .await;

    let (code, _printed, said) = start_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(
        code,
        ExitCode::Internal,
        "its own class, not the refusal code: {said}"
    );
    assert!(
        said.contains("not in effect"),
        "and the operator is told the edits went nowhere: {said}"
    );
}

#[test]
fn a_load_that_did_nothing_to_an_app_says_nothing_about_it() {
    let quiet = SheepApplied::new("zam", Vec::new(), Vec::new(), None);
    assert_eq!(applied_line(&quiet), None);

    let refused = SheepApplied::new(
        "zam",
        Vec::new(),
        Vec::new(),
        Some("zam is not registered".to_string()),
    );
    assert_eq!(
        applied_line(&refused).as_deref(),
        Some("zam: zam is not registered")
    );
}

/// fails if a restart over `--host` reads this machine's dog config: the dog
/// here is adopted locally, its binary would warn, and the restart goes to a
/// shepherd that has never heard of it.
// Unix only for the reason `slow`'s fixture is: `adopted_dog` writes a
// `#!/bin/sh` script, and on Windows the probe this guards would answer
// unknown anyway.
#[cfg(unix)]
#[tokio::test]
async fn a_restart_over_a_host_probes_no_local_dog() {
    let dir = tempfile::tempdir().unwrap();
    let _local = adopted_dog(dir.path(), "log-rotate", &below_floor_answer());
    let sock = shep_client::testing::control_address(dir.path());
    let (client, _daemon) =
        shep_client::testing::fake_client_answering(&sock, |request| match request {
            Request::Restart { .. } => Response::Restarted {
                accepted: Vec::new(),
                refused: Vec::new(),
            },
            _ => Response::Flock(Vec::new()),
        })
        .await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    {
        let mut streams = Streams::bare(&mut out, &mut err, Format::Table);
        let args = SelectorArgs {
            selectors: vec!["log-rotate".into()],
        };
        // Long enough that a probe cannot run out of budget and pass for
        // silence.
        let code =
            restart_within(&client, &mut streams, None, &args, Duration::from_secs(30)).await;
        assert_eq!(code, ExitCode::Success);
    }

    let said = String::from_utf8(err).unwrap();
    assert!(
        !said.contains("dog_binary_skew") && !said.contains("log-rotate"),
        "no local dog is warned about on a restart that goes to a host: {said}"
    );
}

/// Runs `shep reset <selectors>` against `client`, handing back its code
/// and both streams.
async fn reset_against_in(
    client: &Client,
    selectors: &[&str],
    fmt: Format,
) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams::bare(&mut out, &mut err, fmt);
        reset(
            client,
            &mut streams,
            &SelectorArgs {
                selectors: selectors.iter().map(|one| (*one).to_string()).collect(),
            },
        )
        .await
    };
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// fails if `reset` sends anything but one `Reset` per selector, or prints
/// nothing of the flock after it did its work.
#[tokio::test]
async fn reset_sends_one_reset_request_per_selector_and_prints_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&path, |request| match request {
        Request::Reset { .. } => Response::Reset(vec![reloaded_api()]),
        Request::ListFlock => Response::Flock(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reset_against_in(&client, &["api", "7"], Format::Table).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert_eq!(
        envelopes.recv().await.unwrap().body,
        Request::Reset {
            selector: SelectorSpec::Name("api".into())
        }
    );
    assert_eq!(
        envelopes.recv().await.unwrap().body,
        Request::Reset {
            selector: SelectorSpec::Id(7)
        }
    );
    assert!(printed.contains("api"), "the flock rows print: {printed}");
}

/// fails if a shepherd that predates `Reset` stops exiting `Unsupported`, or
/// stops naming the code it answered.
#[tokio::test]
async fn reset_against_a_shepherd_that_predates_it_exits_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _daemon) =
        fake_client_replying_err(&path, RpcErrorCode::Unsupported, "unknown variant").await;

    let (code, printed, said) = reset_against_in(&client, &["api"], Format::Table).await;

    assert_eq!(code, ExitCode::Unsupported, "{said}");
    assert!(said.contains("Unsupported"), "{said}");
    assert!(
        printed.is_empty(),
        "nothing was done, so nothing prints: {printed}"
    );
}

/// fails if `--format json` prints more than one envelope, or files it
/// under a command other than `reset`.
#[tokio::test]
async fn reset_prints_one_json_envelope_under_command_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&path, |request| match request {
        Request::Reset { .. } => Response::Reset(vec![reloaded_api()]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = reset_against_in(&client, &["api"], Format::Json).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    assert_eq!(objects_in(&printed), 1, "{printed}");
    let envelope: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(envelope["command"], "reset", "{envelope}");
    assert_eq!(envelope["data"][0]["name"], "api", "{envelope}");
}
