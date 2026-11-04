//! The first `shep enable` of a built-in dog: it declares itself with no
//! probe and asks like any adopted dog, so the rule has no exceptions.

use shep_core::paths::ShepPaths;

use super::consent::Input;
use super::enable::enable_asking;
use super::test_support::streams;
use crate::commands::shep_toml::ShepToml;
use crate::dog::builtin_manifest;
use crate::exit::ExitCode;

/// A terminal that types `typed`, or no terminal at all.
fn typing(typed: Option<&'static str>) -> Input {
    Input::reading(
        typed.is_some(),
        std::io::Cursor::new(typed.unwrap_or("").as_bytes()),
    )
}

async fn enable(paths: &ShepPaths, name: &str, yes: bool, mut input: Input) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = enable_asking(
        &mut streams(&mut out, &mut err),
        paths,
        name,
        yes,
        &mut input,
    )
    .await;
    (code, String::from_utf8(err).unwrap())
}

/// The grants `shep.toml` records for `name` as names, and whether it is
/// enabled.
fn recorded(paths: &ShepPaths, name: &str) -> (Option<Vec<String>>, bool) {
    let cfg = ShepToml::read_only(&paths.daemon_config).unwrap();
    let granted = cfg
        .granted(name)
        .unwrap()
        .map(|grants| grants.names().map(str::to_owned).collect());
    (
        granted,
        cfg.enabled_dog_names().iter().any(|dog| dog == name),
    )
}

/// What the manifest declares, sorted the way `shep.toml` keeps it.
fn declared(name: &str) -> Vec<String> {
    let mut names: Vec<String> = builtin_manifest(name)
        .expect("a built-in declares itself")
        .permissions
        .into_iter()
        .map(|permission| permission.name)
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn each_built_in_asks_on_its_first_enable_and_a_no_grants_nothing() {
    for name in crate::dog::BUILT_IN_DOGS {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let (_, asked) = enable(&paths, name, false, typing(Some("n\n"))).await;

        assert!(asked.contains("flock.read"), "{name}: {asked}");
        assert!(asked.contains("do not confine"), "{name}: {asked}");
        assert_eq!(recorded(&paths, name), (None, false), "{name}");
    }
}

#[tokio::test]
async fn each_built_in_enabled_with_yes_is_granted_what_it_declares_and_nothing_else() {
    for name in crate::dog::BUILT_IN_DOGS {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());

        let (code, _) = enable(&paths, name, true, typing(None)).await;

        assert_eq!(code, ExitCode::Success, "{name}");
        assert_eq!(
            recorded(&paths, name),
            (Some(declared(name)), true),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_built_in_with_no_terminal_and_no_yes_is_refused_and_not_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());

    let (code, err) = enable(&paths, "metrics", false, typing(None)).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(err.contains("--yes"), "{err}");
    assert!(!paths.daemon_config.exists(), "a refusal writes nothing");
}

#[tokio::test]
async fn a_built_in_already_decided_is_not_asked_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (first, _) = enable(&paths, "metrics", true, typing(None)).await;
    assert_eq!(first, ExitCode::Success);

    let (second, err) = enable(&paths, "metrics", false, typing(None)).await;

    assert_eq!(second, ExitCode::Success, "{err}");
    assert_eq!(
        recorded(&paths, "metrics"),
        (Some(declared("metrics")), true)
    );
}

#[tokio::test]
async fn a_built_in_already_enabled_before_grants_existed_is_granted_its_list_marked_not_reviewed()
{
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(
        &paths.daemon_config,
        "[daemon]\nenabled_dogs = [\"bark\"]\n",
    )
    .unwrap();

    let (code, err) = enable(&paths, "bark", false, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(recorded(&paths, "bark"), (Some(declared("bark")), true));
}

#[test]
fn a_name_that_is_no_built_in_declares_nothing() {
    assert!(builtin_manifest("otel").is_none());
}
