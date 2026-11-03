use shep_client::testing::fake_client_capturing_envelopes;

use super::*;
use crate::cli::Format;
use crate::commands::dogs::test_support::streams;

/// `adopt`'s own report is the only place an operator learns which
/// refusal mode it was.
#[tokio::test]
async fn adopt_of_a_missing_binary_reports_the_refusal_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        yes: true,
        name: Some("otel".to_string()),
        path: dir.path().join("nope"),
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("no file exists at that path"),
        "the refusal must reach the operator: {text}"
    );
}

#[tokio::test]
async fn adopt_asks_the_shepherd_to_start_that_dog_with_its_adopted_source() {
    for channel in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = shep_client::testing::control_address(dir.path());
        let (client, mut envelopes) = fake_client_capturing_envelopes(&path).await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let binary = PathBuf::from("/usr/local/bin/shep-otel");
        let _ = adopt_after_config(
            &mut streams(&mut out, &mut err),
            "adopt",
            "otel",
            DogSource::Adopted {
                grants: None,
                path: binary.display().to_string(),
                channel,
                events: Vec::new(),
            },
            &BTreeMap::new(),
            Some(&client),
        )
        .await;

        let sent = envelopes.recv().await.unwrap();
        assert_eq!(
            sent.body,
            Request::EnableDog {
                name: "otel".to_string(),
                env: BTreeMap::default(),
                source: DogSource::Adopted {
                    grants: None,
                    path: "/usr/local/bin/shep-otel".to_string(),
                    channel,
                    events: Vec::new(),
                },
            }
        );
    }
}

/// Writes an executable `/bin/sh` dog that answers `--version` with
/// `answer` and exits 0 on any other run.
fn write_dog(dir: &Path, answer: &str) -> PathBuf {
    let binary = dir.join("shep-otel");
    let script =
        format!("#!/bin/sh\nif [ \"$1\" = --version ]; then printf '{answer}'; fi\nexit 0\n");
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

/// Through `adopt` itself, so the ask travels from a real probe answer
/// to `shep.toml`. A dog that does not ask is the control.
#[tokio::test]
async fn adopt_records_a_channel_ask_only_when_the_dog_makes_one() {
    for (answer, asked) in [
        (r"shep-otel 0.1.0\nshep-channel: true\n", true),
        (r"shep-otel 0.1.0\n", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let args = AdoptArgs {
            yes: true,
            name: Some("otel".to_string()),
            path: write_dog(dir.path(), answer),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();

        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

        assert_eq!(code, ExitCode::Success);
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
        assert_eq!(
            cfg.daemon.channel_dogs.contains(&"otel".to_string()),
            asked,
            "{written}"
        );
        let notices = String::from_utf8(err).unwrap();
        assert_eq!(
            notices.contains("asked for the shepherd channel"),
            asked,
            "the operator hears what the dog asked for: {notices}"
        );
    }
}

/// `shepherd_acted` and `status` together separate "only the config
/// changed" from "a shepherd acted", and this verb's two branches differ
/// in little else. The pair is the guard: either field alone passes while
/// the other branch carries the wrong one.
#[tokio::test]
async fn adopt_reports_whether_a_shepherd_acted() {
    let dir = tempfile::tempdir().unwrap();
    let path = shep_client::testing::control_address(dir.path());
    let (client, _envelopes) = shep_client::testing::fake_client_answering(&path, |_| {
        Response::DogStarted(shep_client::testing::sample_info())
    })
    .await;
    let binary = PathBuf::from("/usr/local/bin/shep-otel");

    for (client, expected) in [
        (Some(&client), DogActionStatus::Acted("online")),
        (None, NO_SHEPHERD_ENABLE_STATUS),
    ] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut streams = streams(&mut out, &mut err);
        streams.fmt = Format::Json;

        let code = adopt_after_config(
            &mut streams,
            "adopt",
            "otel",
            DogSource::Adopted {
                grants: None,
                path: binary.display().to_string(),
                channel: false,
                events: Vec::new(),
            },
            &BTreeMap::new(),
            client,
        )
        .await;

        assert_eq!(code, ExitCode::Success);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            json["data"]["shepherd_acted"],
            serde_json::json!(expected.shepherd_acted()),
            "client present: {}, payload: {}",
            client.is_some(),
            String::from_utf8_lossy(&out)
        );
        assert_eq!(
            json["data"]["status"],
            expected.text(),
            "payload: {}",
            String::from_utf8_lossy(&out)
        );
    }
}

#[tokio::test]
async fn adopt_with_no_shepherd_writes_the_config_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = dir.path().join("shep-otel");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        yes: true,
        name: Some("otel".to_string()),
        path: binary,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success);
    let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(
        written.contains("otel"),
        "the config edit must still land: {written}"
    );
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("next shepherd"),
        "the operator needs to know the dog is not running yet: {text}"
    );
}

#[tokio::test]
async fn adopt_with_no_name_flag_defaults_from_the_stripped_stem() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = dir.path().join("shep-otel");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        yes: true,
        path: binary,
        name: None,
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::Success);
    let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
    let cfg = shep_core::config::DaemonConfig::load(Some(&written), &|_| None).unwrap();
    assert!(
        cfg.daemon.adopted_dogs.contains_key("otel"),
        "the defaulted name must be `otel`, not `shep-otel`: {written}"
    );
}

/// `dispatch_adopted_dog` (`lib.rs`) runs only once clap has failed to
/// match the name against a real subcommand, so such a dog is
/// unreachable. The refusal must precede any `shep.toml` write.
#[tokio::test]
async fn adopt_refuses_a_name_that_collides_with_a_built_in_verb() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = dir.path().join("watchdog");
    std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();

    let mut out = Vec::new();
    let mut err = Vec::new();
    // "stop" is a real verb; "ls" is `flock`'s own visible alias.
    for reserved in ["stop", "ls"] {
        let args = AdoptArgs {
            yes: true,
            path: binary.clone(),
            name: Some(reserved.to_string()),
        };
        let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;
        assert_eq!(
            code,
            ExitCode::InvalidConfig,
            "`{reserved}` must be refused"
        );
    }
    assert!(
        !paths.daemon_config.exists(),
        "a name collision must never touch shep.toml: {}",
        paths.daemon_config.display()
    );
}

/// The outcome is the same whichever order runs, so the two are told
/// apart by the refusal: `args.path` names nothing on disk, so vet-first
/// would give "no file exists at that path".
#[tokio::test]
async fn a_name_collision_is_refused_before_vet_binary_ever_runs() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());

    let mut out = Vec::new();
    let mut err = Vec::new();
    let args = AdoptArgs {
        yes: true,
        path: dir.path().join("nope"),
        name: Some("stop".to_string()),
    };
    let code = adopt(&mut streams(&mut out, &mut err), &paths, &args).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("already a shep verb or alias"),
        "the collision must be the reported reason, not vet_binary's own refusal: {text}"
    );
    assert!(
        !text.contains("no file exists at that path"),
        "vet_binary must never run on a name that was always going to be refused: {text}"
    );
}

#[test]
fn resolve_adopt_path_prefers_a_literal_path_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("thing");
    std::fs::write(&binary, "").unwrap();

    let resolved = resolve_adopt_path(&binary, None, None);
    assert_eq!(resolved, binary);
}

#[test]
fn resolve_adopt_path_expands_a_leading_tilde_against_the_given_home() {
    let home = tempfile::tempdir().unwrap();
    let binary_dir = home.path().join(".cargo/bin");
    std::fs::create_dir_all(&binary_dir).unwrap();
    let binary = binary_dir.join("shep-log-rotate");
    std::fs::write(&binary, "").unwrap();

    let raw = Path::new("~/.cargo/bin/shep-log-rotate");
    let resolved = resolve_adopt_path(raw, Some(home.path()), None);
    assert_eq!(resolved, binary);
}

/// `exists` answers `false` for a path it cannot search, which would hand
/// `vet_binary` the unexpanded `~/…` and a "no file exists" for a file
/// that is there.
#[test]
fn resolve_adopt_path_expands_a_tilde_path_it_cannot_search() {
    let home = tempfile::tempdir().unwrap();
    let sealed = home.path().join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    std::fs::write(sealed.join("dog"), "").unwrap();
    let mut mode = std::fs::metadata(&sealed).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o000);
    std::fs::set_permissions(&sealed, mode.clone()).unwrap();

    // A process that bypasses mode bits (root) can still search the
    // directory, so the lookup never fails and nothing is under test.
    let hidden = std::fs::metadata(sealed.join("dog")).is_err();
    let raw = Path::new("~/sealed/dog");
    let resolved = resolve_adopt_path(raw, Some(home.path()), None);

    // Restored before any assert so the tempdir can be removed.
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o700);
    std::fs::set_permissions(&sealed, mode).unwrap();
    if !hidden {
        return;
    }
    assert_eq!(resolved, sealed.join("dog"));
}

/// `cargo install shep-log-rotate` puts the binary on `$PATH` under its
/// own name, and nowhere else.
#[test]
fn resolve_adopt_path_falls_back_to_a_path_lookup_for_a_bare_name() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("shep-log-rotate");
    std::fs::write(&binary, "").unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    let path_var = std::ffi::OsString::from(dir.path());

    let raw = Path::new("shep-log-rotate");
    let resolved = resolve_adopt_path(raw, None, Some(&path_var));
    assert_eq!(resolved, binary);
}

/// A same-named file elsewhere on `$PATH` would silently adopt the
/// wrong binary.
#[test]
fn resolve_adopt_path_does_not_path_search_a_name_with_a_directory_component() {
    let path_dir = tempfile::tempdir().unwrap();
    // A file that would match if `$PATH` were searched, so the guard is
    // what this proves rather than an absent file.
    let decoy = path_dir.path().join("thing");
    std::fs::write(&decoy, "").unwrap();
    let mut mode = std::fs::metadata(&decoy).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&decoy, mode).unwrap();
    let path_var = std::ffi::OsString::from(path_dir.path());

    let raw = Path::new("./thing");
    let resolved = resolve_adopt_path(raw, None, Some(&path_var));
    assert_eq!(
        resolved, raw,
        "a name with its own directory must never be searched on $PATH"
    );
}

/// Keeps a plain missing path reporting [`AdoptRefusal::Missing`]
/// rather than a resolution-specific error.
#[test]
fn resolve_adopt_path_returns_raw_unchanged_when_nothing_resolves() {
    let raw = Path::new("/nonexistent/shep-nothing");
    assert_eq!(resolve_adopt_path(raw, None, None), raw);
}

#[test]
fn default_dog_name_strips_one_leading_shep_prefix_and_no_further() {
    assert_eq!(
        default_dog_name(Path::new("/opt/bin/shep-log-rotate")),
        "log-rotate"
    );
    assert_eq!(default_dog_name(Path::new("/opt/bin/otel")), "otel");
    // Stripping the prefix here would leave "", an unreachable name,
    // so the whole stem is kept.
    assert_eq!(default_dog_name(Path::new("/opt/bin/shep-")), "shep-");
}

#[test]
fn collides_with_a_verb_covers_names_and_visible_aliases() {
    assert!(collides_with_a_verb("stop"), "a real verb must collide");
    assert!(collides_with_a_verb("ls"), "flock's own alias must collide");
    // clap generates the `help` subcommand when it builds a command, so
    // it is missing until `Cli::command()` is built. `shep help` prints
    // shep's own help and never reaches a dog.
    assert!(
        collides_with_a_verb("help"),
        "clap's generated help subcommand is a name a dog can never take"
    );
    assert!(
        !collides_with_a_verb("watchdog"),
        "an arbitrary name must not collide"
    );
}
