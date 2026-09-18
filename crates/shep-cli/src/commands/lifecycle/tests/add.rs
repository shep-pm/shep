use super::*;

/// `add` and `start` are one code path, so this guards a mode that
/// leaked rather than a missing feature.
#[tokio::test]
async fn add_registers_a_fresh_app_and_sends_no_start() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"demo\"\nscript = \"/bin/sleep\"\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_that_registers(Vec::new())).await;

    let (code, _out, _err) = add_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    let sent = sent(&mut envelopes);
    let registered: Vec<&str> = sent
        .iter()
        .filter_map(|request| match request {
            Request::Add { apps } => Some(apps[0].name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(registered, vec!["demo"], "one registration, for the app");
    assert!(
        !sent.iter().any(|r| matches!(r, Request::Start { .. })),
        "nothing was started"
    );
}

/// `add` exists so an operator can fill in the empty `env` keys a
/// template shipped, and a key nothing established is one the next load
/// overwrites.
#[tokio::test]
async fn add_establishes_the_keys_the_template_declared() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"demo\"\nscript = \"/bin/sleep\"\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_that_registers(Vec::new())).await;

    let (code, _out, _err) = add_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    let sent = sent(&mut envelopes);
    let add_at = sent
        .iter()
        .position(|r| matches!(r, Request::Add { .. }))
        .expect("the app was registered");
    let apply_at = sent
        .iter()
        .position(|r| matches!(r, Request::ApplyConfig { .. }))
        .expect("its declared keys were established");
    assert!(
        add_at < apply_at,
        "the app is registered before its keys are established"
    );
}

/// Re-running a template after editing it is ordinary, so the file's new
/// keys merge in and the running child survives it.
#[tokio::test]
async fn add_merges_into_a_running_app_without_replacing_it() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"/bin/sleep\"\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_that_registers(a_clustered_flock(&[0]))).await;

    let (code, _out, _err) = add_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    let sent = sent(&mut envelopes);
    assert!(
        sent.iter()
            .any(|r| matches!(r, Request::ApplyConfig { .. })),
        "the template was merged in"
    );
    assert!(
        !sent.iter().any(|r| matches!(r, Request::Restart { .. })),
        "and nothing was replaced"
    );
    assert!(
        !sent.iter().any(|r| matches!(r, Request::Add { .. })),
        "the flock already has it, so there was nothing to register"
    );
}

/// A name target reads no Flockfile, and `add` sits behind that same
/// boundary. Nothing is left for the verb to do but say so.
#[tokio::test]
async fn add_by_name_registers_nothing_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_that_registers(a_clustered_flock(&[0]))).await;

    let (code, _out, said) = add_against(&client, "zam").await;

    assert_eq!(code, ExitCode::Success);
    let sent = sent(&mut envelopes);
    assert!(
        !sent.iter().any(|r| matches!(r, Request::Add { .. })),
        "the flock already has it"
    );
    assert!(
        !sent
            .iter()
            .any(|r| matches!(r, Request::ApplyConfig { .. })),
        "a name target reads no file, so there is nothing to apply"
    );
    assert!(
        said.contains("already registered"),
        "the operator is told why nothing happened: {said}"
    );
}
