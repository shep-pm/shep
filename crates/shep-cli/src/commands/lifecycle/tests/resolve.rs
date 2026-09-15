use super::*;

#[test]
fn an_empty_value_is_an_empty_string_and_not_a_removal() {
    let targets = ["A=".to_string(), "./koji".to_string()];

    let (assignments, _rest) = split_assignments(&targets);

    assert_eq!(assignments.get("A").map(String::as_str), Some(""));
}

#[test]
fn only_the_first_equals_separates_a_name_from_its_value() {
    let targets = ["A=x=y".to_string(), "./koji".to_string()];

    let (assignments, _rest) = split_assignments(&targets);

    assert_eq!(assignments.get("A").map(String::as_str), Some("x=y"));
}

#[test]
fn a_repeated_name_takes_its_last_value() {
    let targets = ["A=1".to_string(), "A=2".to_string(), "./koji".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert_eq!(assignments.get("A").map(String::as_str), Some("2"));
    assert_eq!(rest, ["./koji".to_string()]);
}

#[test]
fn a_path_is_a_target_even_when_it_holds_an_equals_sign() {
    let targets = ["./A=1".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert!(assignments.is_empty(), "a name may not hold a separator");
    assert_eq!(rest, targets);
}

#[test]
fn a_name_holding_a_dash_is_not_an_assignment() {
    let targets = ["A-B=1".to_string(), "./koji".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert!(
        assignments.is_empty(),
        "a name holds letters, digits and `_`"
    );
    assert_eq!(rest, targets);
}

#[test]
fn an_assignment_after_the_target_stays_a_target() {
    let targets = ["./koji".to_string(), "B=2".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert!(
        assignments.is_empty(),
        "the first non-assignment ends the run"
    );
    assert_eq!(rest, targets);
}

#[test]
fn assignments_with_nothing_after_them_leave_no_target() {
    let targets = ["A=1".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert_eq!(assignments.get("A").map(String::as_str), Some("1"));
    assert!(rest.is_empty());
}

#[test]
fn a_name_a_shell_would_reject_is_not_an_assignment() {
    let targets = ["1A=1".to_string(), "./koji".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert!(assignments.is_empty(), "a name may not start with a digit");
    assert_eq!(
        rest, targets,
        "the word stays a target, as a shell leaves it"
    );
}

#[test]
fn leading_assignments_come_off_the_front() {
    let targets = ["A=1".to_string(), "./koji".to_string()];

    let (assignments, rest) = split_assignments(&targets);

    assert_eq!(assignments.get("A").map(String::as_str), Some("1"));
    assert_eq!(rest, ["./koji".to_string()]);
}

#[tokio::test]
async fn a_discovered_flockfile_is_started_when_no_target_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"demo\"\nscript = \"/bin/sleep\"\n",
    )
    .unwrap();

    let sock = shep_client::testing::control_address(dir.path());
    let (client, mut envelopes) = fake_client_capturing_envelopes(&sock).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = StartArgs {
        targets: Vec::new(),
        name: None,
        fold: None,
        cwd: None,
        interpreter: None,
        flockfile: false,
        reset: None,
    };
    {
        let mut streams = Streams {
            out: &mut out,
            err: &mut err,
            style: crate::style::Presentation::BARE,
            fmt: Format::Table,
        };
        let _ = start(
            &client,
            &mut streams,
            &args,
            Some(flockfile.as_path()),
            &BTreeMap::new(),
        )
        .await;
    }

    let sent = next_start(&mut envelopes).await;
    match sent.body {
        Request::Start { apps } => {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].name, "demo");
        }
        other => panic!("expected a Start request, got {other:?}"),
    }
}

#[test]
fn a_flockfile_app_without_a_cwd_runs_where_the_flockfile_lives() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"web\"\nscript = \"./sub/server\"\n",
    )
    .unwrap();

    let apps = resolve_target(flockfile.to_str().unwrap(), None, &[], false)
        .expect("the Flockfile parses");

    // Canonical and without the verbatim prefix, the shape the app is
    // given.
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let expected = shep_core::paths::strip_verbatim_prefix(&canonical).into_owned();
    assert_eq!(
        apps[0].cwd.as_deref(),
        Some(expected.to_string_lossy().as_ref()),
        "the app runs where its Flockfile lives"
    );
}

#[test]
fn a_flockfile_app_that_sets_its_own_cwd_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let flockfile = dir.path().join("Flockfile.toml");
    std::fs::write(
        &flockfile,
        "[[app]]\nname = \"web\"\nscript = \"./server\"\ncwd = \"/srv/elsewhere\"\n",
    )
    .unwrap();

    let apps = resolve_target(flockfile.to_str().unwrap(), None, &[], false)
        .expect("the Flockfile parses");

    assert_eq!(apps[0].cwd.as_deref(), Some("/srv/elsewhere"));
}

#[test]
fn a_relative_script_is_resolved_against_the_callers_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("bin");
    std::fs::create_dir_all(&nested).unwrap();
    let script = nested.join("thing");
    std::fs::write(&script, b"#!/bin/sh\n").unwrap();

    // From the tempdir, the way the CLI resolves from where the
    // operator stands.
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let apps = resolve_target("./bin/thing", None, &[], false);
    std::env::set_current_dir(previous).unwrap();

    let apps = apps.expect("a script that exists must resolve");
    assert_eq!(apps.len(), 1);
    let sent = &apps[0].script;
    assert!(
        std::path::Path::new(sent).is_absolute(),
        "the daemon resolves against its own cwd, so what crosses must be \
             absolute: {sent}"
    );
    assert!(
        std::path::Path::new(sent).exists(),
        "and it must still name the real file: {sent}"
    );
    assert_eq!(apps[0].name, "thing", "the name still comes from the stem");
}

/// Covers `resolve_target`'s pure `-` arm. The real `std::io::stdin()`
/// read inside `start` has no injection seam and is untested.
#[test]
fn a_dash_target_reads_a_flockfile_from_stdin_as_json() {
    // `app`, not `apps`: the wire key is renamed and unknown keys are a
    // hard error.
    let apps = resolve_target(
        "-",
        None,
        br#"{"app":[{"name":"web","script":"./srv"}]}"#,
        false,
    )
    .unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "web");
}

#[test]
fn a_recognised_extension_parses_as_a_flockfile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.toml");
    std::fs::write(&path, "[[app]]\nname = \"web\"\nscript = \"./srv\"\n").unwrap();
    let apps = resolve_target(path.to_str().unwrap(), None, b"", false).unwrap();
    assert_eq!(apps[0].name, "web");
}

#[test]
fn any_other_existing_path_becomes_one_minimal_app_named_for_its_stem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.js");
    std::fs::write(&path, "").unwrap();
    let apps = resolve_target(path.to_str().unwrap(), None, b"", false).unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "server");
    assert_eq!(apps[0].script, path.to_str().unwrap());
}

#[test]
fn an_explicit_name_overrides_the_file_stem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.js");
    std::fs::write(&path, "").unwrap();
    let apps = resolve_target(path.to_str().unwrap(), Some("api"), b"", false).unwrap();
    assert_eq!(apps[0].name, "api");
}

#[test]
fn a_js_file_without_the_flag_is_still_a_script() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.js");
    std::fs::write(&path, "throw new Error('this must never be evaluated')").unwrap();
    let apps = resolve_target(path.to_str().unwrap(), None, b"", false).unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "server");
    assert_eq!(apps[0].script, path.to_str().unwrap());
}

#[test]
fn the_flag_does_not_change_a_toml_flockfile() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.toml");
    std::fs::write(&path, "[[app]]\nname = \"web\"\nscript = \"./srv\"\n").unwrap();
    let with = resolve_target(path.to_str().unwrap(), None, b"", true).unwrap();
    let without = resolve_target(path.to_str().unwrap(), None, b"", false).unwrap();
    assert_eq!(with, without);
}

/// Falling through to the script arm would start the operator's config
/// file as a program.
#[test]
fn the_flag_refuses_an_extension_it_cannot_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.ini");
    std::fs::write(&path, "").unwrap();
    let err = resolve_target(path.to_str().unwrap(), None, b"", true).unwrap_err();
    assert!(matches!(err, TargetError::UnknownFlockfileFormat { .. }));
    assert_eq!(target_exit_code(&err), ExitCode::Usage);
}

#[test]
fn a_js_flockfile_under_the_flag_is_evaluated() {
    if !node_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.js");
    std::fs::write(
        &path,
        "module.exports = { app: [{ name: \"web\", script: \"./srv\" }] };",
    )
    .unwrap();
    let apps = resolve_target(path.to_str().unwrap(), None, b"", true).unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].name, "web");
}

#[test]
fn a_js_flockfile_that_throws_is_an_invalid_config_quoting_node() {
    if !node_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.js");
    std::fs::write(&path, "throw new Error('sheep dip empty');").unwrap();
    let err = resolve_target(path.to_str().unwrap(), None, b"", true).unwrap_err();
    assert_eq!(target_exit_code(&err), ExitCode::InvalidConfig);
    assert!(err.to_string().contains("sheep dip empty"), "got: {err}");
}

/// `setInterval` keeps node's event loop alive after `module.exports` is
/// assigned. 200ms rather than [`JS_EVAL_BUDGET`]: what this pins is that
/// the bound is enforced, not what the shipped bound is.
#[test]
fn a_js_flockfile_that_keeps_node_alive_is_killed_and_says_why() {
    if !node_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flock.js");
    std::fs::write(
        &path,
        "setInterval(() => {}, 1000); module.exports = { app: [] };",
    )
    .unwrap();

    let started = std::time::Instant::now();
    let err = evaluate_js_flockfile(&path, Duration::from_millis(200)).unwrap_err();

    assert_eq!(target_exit_code(&err), ExitCode::InvalidConfig);
    assert!(err.to_string().contains("still running"), "got: {err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "node was waited out rather than killed, in {:?}",
        started.elapsed()
    );
}

/// The refusal has to name the key the operator changes; serde's own
/// message is the answer.
#[test]
fn a_pm2_ecosystem_shape_is_refused_naming_the_right_key() {
    if !node_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ecosystem.config.js");
    std::fs::write(
        &path,
        "module.exports = { apps: [{ name: \"web\", script: \"./srv\" }] };",
    )
    .unwrap();
    let err = resolve_target(path.to_str().unwrap(), None, b"", true).unwrap_err();
    assert_eq!(target_exit_code(&err), ExitCode::InvalidConfig);
    let msg = err.to_string();
    assert!(msg.contains("apps"), "must name what was written: {msg}");
    assert!(msg.contains("app"), "must name what was expected: {msg}");
}

/// Armed through `fake_client_on`: the flock has to answer for the
/// lookup to find anything, and only that fixture lets a test arm the
/// reply.
#[tokio::test]
async fn a_target_naming_a_stopped_sheep_is_acted_on_not_resolved_as_a_path() {
    use shep_client::testing::fake_client_on;
    use shep_core::status::ProcStatus;

    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, daemon) = fake_client_on(&path).await;
    daemon.reply_to_list(vec![
        shep_core::protocol::ProcessInfo::builder(7, "api-auth", ProcStatus::Stopped).build(),
    ]);

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
            &start_args("api-auth"),
            None,
            &BTreeMap::new(),
        )
        .await
    };

    // The path arms would have refused: no file by that name exists here.
    assert_ne!(
        code,
        ExitCode::Usage,
        "a known name must not fall through to the path arms: {}",
        String::from_utf8_lossy(&err)
    );
    assert!(
        !String::from_utf8_lossy(&err).contains("api-auth\" does not"),
        "and must not be reported as an unresolvable target"
    );
}
