use super::*;

/// Collapsing matched rows to distinct names would widen the request:
/// `Id` is the only selector form that names a subset of one name's rows.
#[tokio::test]
async fn a_start_by_id_respawns_that_row_and_no_other() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[]), &[])).await;

    let (code, _, _) = start_against(&client, "0").await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        respawns(&mut envelopes),
        vec![SelectorSpec::Id(0)],
        "one respawn, for the row the operator named"
    );
}

/// Asserts on the declared key set as well as the name, because that is
/// what a merge keys on: an empty set puts the same envelope on the wire
/// and applies nothing.
#[tokio::test]
async fn a_flockfile_load_applies_its_declared_keys_to_an_app_the_flock_has() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"./srv\"\nmax_restarts = 99\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let (code, _printed, _said) = start_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    let mut sent = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::ApplyConfig { apps, reset } = envelope.body {
            sent.push((apps, reset));
        }
    }
    assert_eq!(sent.len(), 1, "one request for the whole invocation");
    let (apps, reset) = &sent[0];
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].config.name, "zam");
    assert!(
        apps[0].declared.contains("max_restarts"),
        "the edited key is declared: {:?}",
        apps[0].declared
    );
    assert_eq!(
        *reset,
        ResetDepth::None,
        "additive by default; --reset is something the operator types"
    );
}

/// `reset_depth` is the only place that mapping happens.
#[tokio::test]
async fn reset_modes_choose_the_apply_config_depth_on_the_wire() {
    use shep_client::testing::fake_client_answering;

    async fn sent_depth(reset: Option<ResetMode>) -> ResetDepth {
        let dir = tempfile::tempdir().unwrap();
        let flockfile = dir.path().join("Flockfile.toml");
        std::fs::write(
            &flockfile,
            "[[app]]\nname = \"zam\"\nscript = \"./srv\"\nmax_restarts = 99\n",
        )
        .unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) =
            fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;
        let mut args = start_args(flockfile.to_str().unwrap());
        args.reset = reset;
        let (code, _printed, _said) = start_against_with_args(&client, &args).await;
        assert_eq!(code, ExitCode::Success);
        let mut sent = Vec::new();
        while let Ok(envelope) = envelopes.try_recv() {
            if let Request::ApplyConfig { reset, .. } = envelope.body {
                sent.push(reset);
            }
        }
        assert_eq!(sent.len(), 1, "one request for the whole invocation");
        sent[0]
    }

    assert_eq!(sent_depth(None).await, ResetDepth::None);
    assert_eq!(sent_depth(Some(ResetMode::File)).await, ResetDepth::File);
    assert_eq!(
        sent_depth(Some(ResetMode::Policy)).await,
        ResetDepth::Policy
    );
    assert_eq!(sent_depth(Some(ResetMode::Env)).await, ResetDepth::Env);
    assert_eq!(sent_depth(Some(ResetMode::All)).await, ResetDepth::All);
}

/// There is no file to reset to, so the flag is meaningless.
#[tokio::test]
async fn a_reset_flag_on_a_name_target_is_refused() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let mut args = start_args("zam");
    args.reset = Some(ResetMode::Env);
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("--reset=env") && said.contains("zam"),
        "the refusal must echo the mode the operator actually typed, \
             not a bare --reset that is now its own usage error: {said}"
    );
}

/// The command line is not a template, so the flag would otherwise exit
/// 0 having reset nothing.
#[tokio::test]
async fn a_reset_flag_on_a_bare_script_target_is_refused() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let mut args = start_args(script.to_str().unwrap());
    args.reset = Some(ResetMode::All);
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("--reset=all") && said.contains("zam"),
        "the refusal must echo the mode the operator actually typed, \
             not a bare --reset that is now its own usage error: {said}"
    );
    assert!(
        applies(&mut envelopes).is_empty(),
        "a refused reset sends no load"
    );
}

/// The command line supplied every value, including a `cwd` of wherever
/// the operator stood, so applying it would move a running app's
/// directory.
#[tokio::test]
async fn an_assignment_is_recorded_as_an_operator_override() {
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();

    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&sock, |request: &Request| match request {
        Request::ListFlock => Response::Flock(Vec::new()),
        Request::Start { apps } => Response::Started(
            apps.iter()
                .map(|app| ProcessInfo::builder(0, app.name.as_str(), ProcStatus::Online).build())
                .collect(),
        ),
        Request::SetSheepEnv { name, key, .. } => Response::SheepEnvSet {
            name: name.clone(),
            key: key.clone(),
        },
        _ => Response::Pong,
    })
    .await;

    let mut args = start_args("KOJI_TOKEN=s3cret");
    args.targets.push(script.to_string_lossy().into_owned());
    let (code, _printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    let mut recorded = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        if let Request::SetSheepEnv { name, key, value } = envelope.body {
            recorded.push((name, key, value.map(|v| v.as_str().to_string())));
        }
    }
    assert_eq!(
        recorded,
        vec![(
            "zam".to_string(),
            "KOJI_TOKEN".to_string(),
            Some("s3cret".to_string())
        )],
        "without the override record a later Flockfile load overwrites the value"
    );
}

#[tokio::test]
async fn an_assignment_reaches_the_env_of_the_sheep_it_registers() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();

    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&sock).await;
    let mut args = start_args("KOJI_TOKEN=s3cret");
    args.targets.push(script.to_string_lossy().into_owned());
    let _ = start_against_with_args(&client, &args).await;

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(
                apps[0].env.get("KOJI_TOKEN").map(String::as_str),
                Some("s3cret"),
                "the first spawn must already carry the value"
            );
        }
        other => panic!("expected a Start request, got {other:?}"),
    }
}

#[tokio::test]
async fn an_assignment_on_an_existing_sheep_is_recorded_before_it_resumes() {
    use shep_client::testing::fake_client_answering;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let flock = vec![ProcessInfo::builder(0, "zam", ProcStatus::Stopped).build()];
    let (client, mut envelopes) =
        fake_client_answering(&sock, move |request: &Request| match request {
            Request::ListFlock => Response::Flock(flock.clone()),
            Request::SetSheepEnv { name, key, .. } => Response::SheepEnvSet {
                name: name.clone(),
                key: key.clone(),
            },
            Request::Restart { .. } => Response::Restarted {
                accepted: vec![ProcessInfo::builder(0, "zam", ProcStatus::Online).build()],
                refused: Vec::new(),
            },
            _ => Response::Pong,
        })
        .await;

    let mut args = start_args("A=1");
    args.targets.push("zam".to_string());
    let (code, _printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Success, "{said}");
    let mut order = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        match envelope.body {
            Request::SetSheepEnv { key, .. } => order.push(format!("set {key}")),
            Request::Restart { .. } => order.push("restart".to_string()),
            _ => {}
        }
    }
    assert_eq!(
        order,
        ["set A", "restart"],
        "the value must be recorded before the sheep comes back up"
    );
}

#[tokio::test]
async fn an_assignment_on_a_target_matching_several_sheep_is_refused() {
    use shep_client::testing::fake_client_answering;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let flock = vec![
        ProcessInfo::builder(0, "web", ProcStatus::Online).build(),
        ProcessInfo::builder(1, "api", ProcStatus::Online).build(),
    ];
    let (client, mut envelopes) = fake_client_answering(&sock, a_daemon_for(flock, &[])).await;

    let mut args = start_args("A=1");
    args.targets.push("all".to_string());
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("one sheep"),
        "the refusal must say an environment belongs to one sheep: {said}"
    );
    assert!(
        respawns(&mut envelopes).is_empty(),
        "a refused assignment restarts nothing"
    );
}

#[tokio::test]
async fn an_assignment_reaches_the_env_of_a_sheep_add_registers() {
    use shep_client::testing::fake_client_answering;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&sock, |request: &Request| match request {
        Request::ListFlock => Response::Flock(Vec::new()),
        Request::Add { apps } => Response::Added(
            apps.iter()
                .map(|app| ProcessInfo::builder(0, app.name.as_str(), ProcStatus::Stopped).build())
                .collect(),
        ),
        Request::SetSheepEnv { name, key, .. } => Response::SheepEnvSet {
            name: name.clone(),
            key: key.clone(),
        },
        _ => Response::Pong,
    })
    .await;

    let mut args = start_args("KOJI_TOKEN=s3cret");
    args.targets.push(script.to_string_lossy().into_owned());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        add(&client, &mut streams, &args, None, &BTreeMap::new()).await
    };
    assert_eq!(code, ExitCode::Success, "{}", String::from_utf8_lossy(&err));

    let mut registered = None;
    let mut recorded = Vec::new();
    while let Ok(envelope) = envelopes.try_recv() {
        match envelope.body {
            Request::Add { apps } => {
                registered = apps
                    .first()
                    .and_then(|app| app.env.get("KOJI_TOKEN").cloned());
            }
            Request::SetSheepEnv { key, .. } => recorded.push(key),
            _ => {}
        }
    }
    assert_eq!(
        registered.as_deref(),
        Some("s3cret"),
        "add registers the value it was given"
    );
    assert_eq!(
        recorded,
        ["KOJI_TOKEN"],
        "and records it as an override, exactly as start does"
    );
}

#[test]
fn a_target_that_looks_like_an_assignment_says_why_it_is_not_one() {
    let said = TargetError::Unresolvable {
        target: "1A=1".to_string(),
    }
    .to_string();

    assert!(
        said.contains("`1A`"),
        "the refusal names the bad name: {said}"
    );
    assert!(
        said.contains("letter"),
        "and says what a name may hold: {said}"
    );
}

#[test]
fn a_path_holding_an_equals_sign_gets_no_assignment_note() {
    let said = TargetError::Unresolvable {
        target: "./A=1".to_string(),
    }
    .to_string();

    assert!(
        !said.contains("letter"),
        "a path was never a candidate assignment: {said}"
    );
}

#[tokio::test]
async fn a_quoted_assignment_and_target_says_to_drop_the_quotes() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&sock, a_daemon_for(Vec::new(), &[])).await;

    let args = start_args("ABC=xyz /tmp/x/koji");
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("drop the quotes"),
        "one quoted word is the shape a shell never produces: {said}"
    );
}

#[tokio::test]
async fn an_assignment_with_no_target_at_all_is_refused() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = fake_client_answering(&sock, a_daemon_for(Vec::new(), &[])).await;

    let args = start_args("A=1");
    let (code, _printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(
        said.contains("needs a target"),
        "an environment with nothing to set it on: {said}"
    );
}

#[tokio::test]
async fn an_assignment_on_a_flockfile_is_refused() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"web\"\nscript = \"/bin/sleep\"\n",
    )
    .unwrap();
    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_answering(&sock, a_daemon_for(Vec::new(), &[])).await;

    let mut args = start_args("A=1");
    args.targets.push(flockfile.to_string_lossy().into_owned());
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("Flockfile"),
        "the refusal must say a file may declare several sheep: {said}"
    );
    assert!(
        applies(&mut envelopes).is_empty(),
        "a refused assignment loads nothing"
    );
}

#[tokio::test]
async fn an_assignment_with_more_than_one_target_is_refused() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let mut args = start_args("A=1");
    args.targets.push(script.to_string_lossy().into_owned());
    args.targets.push(script.to_string_lossy().into_owned());
    let (code, printed, said) = start_against_with_args(&client, &args).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(printed.is_empty(), "a refusal prints no data envelope");
    assert!(
        said.contains("one target"),
        "the refusal must say an environment belongs to one sheep: {said}"
    );
    assert!(
        applies(&mut envelopes).is_empty(),
        "a refused assignment loads nothing"
    );
}

#[tokio::test]
async fn a_bare_script_target_applies_nothing() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("zam");
    std::fs::write(&script, "#!/bin/sh\nsleep 1\n").unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let (code, _printed, _said) = start_against(&client, script.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        applies(&mut envelopes).is_empty(),
        "a script path declares nothing, so there is nothing to apply"
    );
}

/// A list of names nobody can act on is a report nobody can use.
#[tokio::test]
async fn a_load_names_the_verb_that_promotes_what_is_pending() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"./srv\"\nmax_restarts = 99\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let flock = a_clustered_flock(&[0, 1, 2]);
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::ListFlock => Response::Flock(flock.clone()),
        Request::ApplyConfig { .. } => Response::Applied(vec![SheepApplied::new(
            "zam",
            vec!["max_restarts".to_string()],
            vec!["env".to_string()],
            None,
        )]),
        _ => Response::Pong,
    })
    .await;

    let (code, _printed, said) = start_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        said.contains("applied max_restarts"),
        "what landed is named: {said}"
    );
    assert!(
        said.contains("shep reload zam"),
        "and what promotes the rest: {said}"
    );
}

/// Both apps are in one reply: one refusal among successes still fails
/// the verb, and the app that did apply is still reported.
#[tokio::test]
async fn a_load_that_refused_an_app_exits_non_zero_and_still_reports_the_rest() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"./srv\"\nmax_restarts = 99\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let flock = a_clustered_flock(&[0, 1, 2]);
    let (client, _envelopes) = fake_client_answering(&path, move |request| match request {
        Request::ListFlock => Response::Flock(flock.clone()),
        Request::ApplyConfig { .. } => Response::Applied(vec![
            SheepApplied::new("zam", vec!["max_restarts".to_string()], Vec::new(), None),
            SheepApplied::new(
                "api",
                Vec::new(),
                Vec::new(),
                Some("instances: this load never reshapes a flock".to_string()),
            ),
        ]),
        _ => Response::Pong,
    })
    .await;

    let (code, printed, said) = start_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(
        code,
        ExitCode::InvalidConfig,
        "a refused load is a failed load: {said}"
    );
    assert!(
        said.contains("never reshapes a flock"),
        "the refusal reaches the operator: {said}"
    );
    assert!(
        said.contains("applied max_restarts"),
        "and so does what did land, beside it: {said}"
    );
    assert!(
        printed.is_empty(),
        "a failed verb leaves stdout empty, so `--format json` never \
             carries a data envelope beside an error one: {printed}"
    );
}
