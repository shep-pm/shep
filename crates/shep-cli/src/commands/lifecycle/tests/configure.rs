use super::*;

/// `Flockfile::parse_declared` deserializes the source a second time
/// into a `serde_json::Value`. One case per format, each carrying a
/// nested table and a list, the two shapes a generic pass is most likely
/// to handle differently from a typed one.
#[test]
fn every_format_survives_the_second_deserialize_resolve_target_now_runs() {
    let dir = tempfile::tempdir().unwrap();
    let sources = [
        (
            "Flockfile.toml",
            "[[app]]\nname = \"web\"\nscript = \"./srv\"\nargs = [\"-p\", \"80\"]\n\
                 [app.env]\nMODE = \"live\"\n",
        ),
        (
            "Flockfile.yaml",
            "app:\n  - name: web\n    script: ./srv\n    args: ['-p', '80']\n\
                 \n    env:\n      MODE: live\n",
        ),
        (
            "Flockfile.json",
            "{\"app\":[{\"name\":\"web\",\"script\":\"./srv\",\
                 \"args\":[\"-p\",\"80\"],\"env\":{\"MODE\":\"live\"}}]}",
        ),
        (
            "Flockfile.json5",
            "{app:[{name:'web',script:'./srv',args:['-p','80'],env:{MODE:'live'}}]}",
        ),
    ];
    for (filename, source) in sources {
        let path = dir.path().join(filename);
        std::fs::write(&path, source).unwrap();
        let target = path.to_str().unwrap();

        // The validating pass alone, the baseline the second must not
        // narrow.
        let format = FlockFormat::from_path(&path).expect("a recognised extension");
        let first = Flockfile::parse(source, format).expect("the validating pass accepts it");

        let apps = resolve_target(target, None, b"", false)
            .unwrap_or_else(|err| panic!("{filename} was refused after the first pass: {err}"));
        assert_eq!(
            apps.len(),
            first.apps.len(),
            "{filename}: both passes see the same apps"
        );
        assert_eq!(apps[0].name, "web", "{filename}");
        assert_eq!(
            apps[0].args,
            vec!["-p".to_string(), "80".to_string()],
            "{filename}"
        );
        assert_eq!(
            apps[0].env.get("MODE").map(String::as_str),
            Some("live"),
            "{filename}: a nested table survives both passes"
        );
    }
}

/// A bare start already reads the file to decide what to run, so it
/// extends no trust the invocation had not extended already. The only
/// case here that reaches the apply through `discovered`.
#[tokio::test]
async fn a_discovered_flockfile_applies_to_an_app_the_flock_already_has() {
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

    let args = StartArgs {
        targets: Vec::new(),
        name: None,
        fold: None,
        cwd: None,
        interpreter: None,
        flockfile: false,
        reset: None,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(
            &client,
            &mut streams,
            &args,
            Some(flockfile.as_path()),
            &BTreeMap::new(),
        )
        .await
    };

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        applies(&mut envelopes),
        vec![vec!["zam".to_string()]],
        "a discovered load applies exactly as an explicit one does"
    );
}

/// A Flockfile arrives from an app's own repository, so a load is
/// something the operator asked for by naming a file. The Flockfile here
/// declares the same app under a different script, so a build that read
/// it would have something to send.
#[tokio::test]
async fn start_by_name_sends_no_apply_config_even_with_a_flockfile_present() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"./from-the-repository\"\n",
    )
    .unwrap();
    let path = shep_client::testing::control_address(dir.path());
    // Every instance online, so the only requests this invocation can
    // produce are the listings.
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0, 1, 2]), &[])).await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(
            &client,
            &mut streams,
            &start_args("zam"),
            Some(&flockfile),
            &BTreeMap::new(),
        )
        .await
    };

    assert_eq!(code, ExitCode::Success, "the name resolved to the flock");
    assert!(
        applies(&mut envelopes).is_empty(),
        "a name target applies nothing"
    );
}

/// `resume_all` partitions matched rows into live and asleep. Sending a
/// name walks back over the row it just set aside, no `Id` needed.
#[tokio::test]
async fn a_start_never_respawns_a_row_that_was_already_up() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0]), &[])).await;

    let (code, printed, said) = start_against(&client, "all").await;

    assert_eq!(code, ExitCode::Success);
    let _ = printed;
    assert_eq!(
        respawns(&mut envelopes),
        vec![SelectorSpec::Id(1), SelectorSpec::Id(2)],
        "the two that were down, and not the one that was up"
    );
    assert!(said.contains("already"), "the live one is reported: {said}");
}

/// `shep restart zam` for a `shep start 0` replaces every instance. Only
/// a path or Flockfile target falls back to the name.
#[tokio::test]
async fn the_already_up_notice_quotes_the_operators_own_token() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[0]), &[])).await;

    let (code, _, said) = start_against(&client, "0").await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        said.contains("`shep restart 0`"),
        "the one row the operator named: {said}"
    );
    assert!(
        !said.contains("`shep restart zam`"),
        "never the name, which would replace every instance of it: {said}"
    );
}

#[tokio::test]
async fn a_row_that_cannot_spawn_does_not_abandon_the_rows_after_it() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&path, a_daemon_for(a_clustered_flock(&[]), &[0])).await;

    let (code, printed, said) = start_against(&client, "all").await;

    assert_eq!(code, ExitCode::SpawnFailed, "the first failure is the code");
    assert_eq!(
        respawns(&mut envelopes),
        vec![
            SelectorSpec::Id(0),
            SelectorSpec::Id(1),
            SelectorSpec::Id(2)
        ],
        "every row is attempted, not just the ones before the failure"
    );
    assert!(
        said.contains("id 0"),
        "and the failure names the row, not just the app: {said}"
    );
    assert!(
        printed.is_empty(),
        "a failed verb leaves stdout empty even though rows 1 and 2 came \
             up, the rule `cli_e2e`'s assert_json_error pins crate-wide: \
             {printed}"
    );
}

/// Both halves: three rows respawned, and none skipped because the
/// representative row happened to be the live one.
#[tokio::test]
async fn a_flockfile_naming_a_clustered_app_resumes_every_instance() {
    use shep_client::testing::fake_client_answering;

    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("flock.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"zam\"\nscript = \"./zam\"\ninstances = 3\n",
    )
    .unwrap();
    let socket = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) =
        fake_client_answering(&socket, a_daemon_for(a_clustered_flock(&[]), &[])).await;

    let (code, _, _) = start_against(&client, flockfile.to_str().unwrap()).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        respawns(&mut envelopes),
        vec![
            SelectorSpec::Id(0),
            SelectorSpec::Id(1),
            SelectorSpec::Id(2)
        ],
        "every row the name has, not the first one"
    );
}

#[tokio::test]
async fn a_target_that_matches_nothing_is_a_usage_error_naming_what_was_tried() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        start(
            &client,
            &mut streams,
            &start_args("./does-not-exist"),
            None,
            &BTreeMap::new(),
        )
        .await
    };
    assert_eq!(code, ExitCode::Usage);
    // Nothing reaches the daemon: `./does-not-exist` carries a path
    // separator, so `start` skips the flock lookup. A target with no
    // separator does ask.
    assert!(
        envelopes.try_recv().is_err(),
        "a target that can only be a path costs no round trip, and an \
             unresolvable one must never become a Start"
    );
    assert!(String::from_utf8(err).unwrap().contains("./does-not-exist"));
}

/// Every selector-taking verb must send a compiled `SelectorSpec` inside
/// its own `Request` variant
///
/// The whole `sent.body` is asserted, so a verb sending the wrong request
/// kind is caught. Also pins each verb's budget: `stop` and `delete`
/// pass `None` and reach the wire as `DEFAULT_DEADLINE`, while `restart`
/// and `reload` ask for their own because their staged walk runs inside
/// the request handler. `reload` asks for the larger of the two: its
/// stages cost a drain as well as a readiness wait, and two of them
/// clear `START_DEADLINE` at the default timeouts.
#[tokio::test]
async fn a_selector_reaches_the_wire_in_its_compiled_form() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
    // No `shep.toml` here, so `restart`'s dog check finds nothing
    // adopted and spawns nothing.
    let paths = ShepPaths::resolve(&|_| None, dir.path());

    #[derive(Clone, Copy, Debug)]
    enum Verb {
        Stop,
        Restart,
        Reload,
        Delete,
    }

    for verb in [Verb::Stop, Verb::Restart, Verb::Reload, Verb::Delete] {
        for (input, expected) in [
            ("all", SelectorSpec::All),
            ("7", SelectorSpec::Id(7)),
            ("web", SelectorSpec::Name("web".into())),
            ("/^web-/", SelectorSpec::Regex("^web-".into())),
            ("fold:api", SelectorSpec::Fold("api".into())),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut streams = Streams {
                out: &mut out,
                err: &mut err,
                style: crate::style::Presentation::BARE,
                fmt: Format::Table,
            };
            let args = SelectorArgs {
                selectors: vec![input.into()],
            };
            let expected_body = match verb {
                Verb::Stop => Request::Stop { selector: expected },
                Verb::Restart => Request::Restart { selector: expected },
                Verb::Reload => Request::Reload { selector: expected },
                Verb::Delete => Request::Delete { selector: expected },
            };
            let _ = match verb {
                Verb::Stop => stop(&client, &mut streams, &args).await,
                Verb::Restart => restart(&client, &mut streams, &paths, &args).await,
                Verb::Reload => reload(&client, &mut streams, &args).await,
                Verb::Delete => delete(&client, &mut streams, &args).await,
            };
            let sent = envelopes.recv().await.unwrap();
            assert_eq!(sent.body, expected_body, "verb={verb:?} input={input}");
            // fails if a staged verb goes out on the client's 5s
            // default: `restart` and `reload` now walk the dependency
            // stages inside the request handler, and one edge clears 5s,
            // so the client would abandon a walk the shepherd is still
            // doing. `request_with_deadline` fills in `DEFAULT_DEADLINE`
            // for a `None`, so an envelope carrying exactly that is the
            // signal that the call site passed one.
            let expected_deadline = match verb {
                Verb::Stop | Verb::Delete => DEFAULT_DEADLINE,
                Verb::Restart => START_DEADLINE,
                Verb::Reload => RELOAD_DEADLINE,
            };
            assert_eq!(
                sent.deadline_ms,
                Some(u64::try_from(expected_deadline.as_millis()).unwrap()),
                "verb={verb:?} input={input} must ask for its own budget"
            );
        }
    }
}
