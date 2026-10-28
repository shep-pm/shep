//! What `shep dogs grant` and `shep dogs revoke` leave in `shep.toml`, through
//! the verbs with a real `/bin/sh` dog and no shepherd running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use shep_client::testing::{sample_info, serve_one_request};
use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;
use shep_core::protocol::{DogSource, Envelope, Request, Response};

use super::grant::{Change, change};
use super::test_support::{streams, this_builds_ack};
use crate::commands::shep_toml::ShepToml;
use crate::exit::ExitCode;

const DECLARES_TWO: &str = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"},{"name":"events:process.","required":false,"reason":"hears when a sheep exits"}]}"#;

fn write_dog(dir: &Path, manifest: &str) -> PathBuf {
    let binary = dir.join("shep-otel");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n"
    );
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

/// A home with `otel` adopted and enabled, holding `granted` (`None` for no
/// `dog_grants` entry at all).
fn home_with(
    manifest: &str,
    granted: Option<&[&str]>,
    enabled: bool,
) -> (tempfile::TempDir, ShepPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), manifest);
    let mut text = format!("[daemon.adopted_dogs]\notel = \"{}\"\n", binary.display());
    if enabled {
        text.push_str("[daemon]\nenabled_dogs = [\"otel\"]\n");
    }
    if let Some(names) = granted {
        let names: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
        text.push_str(&format!(
            "[daemon.dog_grants.otel]\ngranted = [{}]\n",
            names.join(", ")
        ));
    }
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.daemon_config, text).unwrap();
    (dir, paths)
}

/// The names `shep.toml` records for `otel`, or `None` for no entry.
fn recorded(paths: &ShepPaths) -> Option<Vec<String>> {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .granted("otel")
        .unwrap()
        .map(|grants| grants.names().map(str::to_owned).collect())
}

async fn run(verb: Change, paths: &ShepPaths, permission: &str) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = change(
        &mut streams(&mut out, &mut err),
        paths,
        verb,
        "otel",
        permission,
    )
    .await;
    (code, String::from_utf8(err).unwrap())
}

#[tokio::test]
async fn a_revoke_drops_the_permission_and_keeps_the_rest() {
    let (_dir, paths) = home_with(DECLARES_TWO, Some(&["events:process.", "flock.read"]), true);

    let (code, _) = run(Change::Revoke, &paths, "flock.read").await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(recorded(&paths), Some(vec!["events:process.".to_string()]));
}

#[tokio::test]
async fn a_grant_adds_a_declared_permission_to_what_is_held() {
    let (_dir, paths) = home_with(DECLARES_TWO, Some(&["flock.read"]), true);

    let (code, _) = run(Change::Grant, &paths, "events:process.").await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        recorded(&paths),
        Some(vec![
            "events:process.".to_string(),
            "flock.read".to_string()
        ])
    );
}

#[tokio::test]
async fn a_grant_the_dog_never_declared_is_refused_and_writes_nothing() {
    let (_dir, paths) = home_with(DECLARES_TWO, Some(&["flock.read"]), true);
    let before = std::fs::read_to_string(&paths.daemon_config).unwrap();

    let (code, said) = run(Change::Grant, &paths, "flock.control").await;

    assert_eq!(code, ExitCode::Usage);
    assert!(
        said.contains("otel did not declare `flock.control`"),
        "{said}"
    );
    assert!(said.contains("events:process., flock.read"), "{said}");
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        before
    );
}

#[tokio::test]
async fn a_dog_that_declares_nothing_can_be_granted_nothing() {
    let (_dir, paths) = home_with(
        r#"{"manifest":1,"name":"otel","version":"1","protocol":12}"#,
        Some(&[]),
        true,
    );

    let (code, said) = run(Change::Grant, &paths, "flock.read").await;

    assert_eq!(code, ExitCode::Usage);
    assert!(said.contains("otel declares no permissions"), "{said}");
    assert_eq!(recorded(&paths), Some(vec![]));
}

/// Runs the change with a shepherd listening, and hands back what it was
/// sent, if anything.
async fn run_with_shepherd(
    verb: Change,
    paths: &ShepPaths,
    permission: &str,
) -> (ExitCode, String, tokio::task::JoinHandle<Envelope>) {
    std::fs::create_dir_all(&paths.run).unwrap();
    let handle = serve_one_request(
        &paths.socket,
        this_builds_ack(),
        Response::DogStarted(sample_info()),
    )
    .await;
    let (code, said) = run(verb, paths, permission).await;
    (code, said, handle)
}

/// The acceptance criterion: a revoke reaches the shepherd that is running
/// the dog, as the grants the dog now holds, so its next request is checked
/// against them with no restart.
#[tokio::test]
async fn a_revoke_on_an_enabled_dog_sends_the_shepherd_the_grants_now_held() {
    let (dir, paths) = home_with(DECLARES_TWO, Some(&["events:process.", "flock.read"]), true);

    let (code, _, handle) = run_with_shepherd(Change::Revoke, &paths, "flock.read").await;

    assert_eq!(code, ExitCode::Success);
    let sent = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the revoke must reach the wire; it hung instead of connecting")
        .unwrap();
    assert_eq!(
        sent.body,
        Request::EnableDog {
            name: "otel".to_string(),
            env: BTreeMap::default(),
            source: DogSource::Adopted {
                path: dir.path().join("shep-otel").display().to_string(),
                channel: false,
                grants: Some(Grants::parse(["events:process."]).unwrap()),
            },
        }
    );
}

/// An enable would start a dog the operator has switched off.
#[tokio::test]
async fn a_change_to_a_disabled_dog_is_recorded_without_waking_the_shepherd() {
    let (_dir, paths) = home_with(DECLARES_TWO, Some(&["flock.read"]), false);

    let (code, _, handle) = run_with_shepherd(Change::Revoke, &paths, "flock.read").await;

    assert_eq!(code, ExitCode::Success);
    assert!(!handle.is_finished(), "a disabled dog must not be sent to");
    assert_eq!(recorded(&paths), Some(vec![]));
}

#[tokio::test]
async fn a_misspelt_permission_is_refused_for_both_verbs_and_writes_nothing() {
    let (_dir, paths) = home_with(DECLARES_TWO, Some(&["flock.read"]), true);
    let before = std::fs::read_to_string(&paths.daemon_config).unwrap();

    for verb in [Change::Grant, Change::Revoke] {
        let (code, said) = run(verb, &paths, "flock.reed").await;

        assert_eq!(code, ExitCode::Usage);
        assert!(
            said.contains("`flock.reed` is not a permission shep knows"),
            "{said}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        before
    );
}

#[tokio::test]
async fn a_name_that_is_no_dog_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());

    let (code, said) = run(Change::Revoke, &paths, "flock.read").await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(said.contains("`otel` is not a dog"), "{said}");
}

/// No entry is an undeclared dog with full access, or one never asked: either
/// way one name cannot be carved out of it, and a grant would narrow it to
/// that name alone.
#[tokio::test]
async fn a_dog_with_no_recorded_grants_is_left_alone_by_both_verbs() {
    // Disabled: an enabled dog with a manifest and no entry is a home from
    // before grants, and is granted its list at the first look.
    let (_dir, paths) = home_with(DECLARES_TWO, None, false);
    let before = std::fs::read_to_string(&paths.daemon_config).unwrap();

    for verb in [Change::Grant, Change::Revoke] {
        let (code, said) = run(verb, &paths, "flock.read").await;

        assert_eq!(code, ExitCode::Usage);
        assert!(said.contains("otel has no recorded grants"), "{said}");
    }
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        before
    );
}

#[tokio::test]
async fn revoking_what_is_not_held_changes_nothing_and_succeeds() {
    let declares_one = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"}]}"#;
    let (_dir, paths) = home_with(declares_one, Some(&["flock.read"]), true);
    let before = std::fs::read_to_string(&paths.daemon_config).unwrap();

    let (code, _) = run(Change::Revoke, &paths, "events:process.").await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        before
    );
}

/// A record of the manifest that cannot be read back is a declaration unread,
/// not a dog that declares nothing, so the grant says which and writes nothing.
#[tokio::test]
async fn a_grant_is_not_judged_by_a_manifest_record_that_cannot_be_read() {
    let (dir, paths) = home_with(DECLARES_TWO, Some(&["flock.read"]), true);
    let binary = dir.path().join("shep-otel");
    // Answers once, so the first read files the record, and is silent after.
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ] && [ ! -e \"$0.asked\" ]; then touch \"$0.asked\"; cat <<'EOF'\n{DECLARES_TWO}\nEOF\nfi\nexit 0\n"
    );
    std::fs::write(&binary, script).unwrap();
    let (code, _) = run(Change::Grant, &paths, "flock.read").await;
    assert_eq!(code, ExitCode::Success);
    let record = std::fs::read_dir(paths.home.join("manifests"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(record, "{ truncated").unwrap();
    let before = std::fs::read_to_string(&paths.daemon_config).unwrap();

    let (code, said) = run(Change::Grant, &paths, "events:process.").await;

    assert_eq!(code, ExitCode::Usage);
    assert!(
        said.contains("otel's manifest record could not be read"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.daemon_config).unwrap(),
        before
    );
}
