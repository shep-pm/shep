//! What `shep adopt` and the first `shep enable` leave in `shep.toml` once the
//! operator has answered, through the verbs themselves with real `/bin/sh`
//! dogs and a canned answer in place of a terminal.

use std::io::BufRead;
use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;
use shep_core::permissions::Grants;

use super::adopt::adopt_asking;
use super::consent::Input;
use super::enable::enable_asking;
use super::test_support::streams;
use crate::cli::AdoptArgs;
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

/// A terminal that types `typed`, or no terminal at all.
fn typing(typed: Option<&'static str>) -> Input {
    Input {
        is_terminal: typed.is_some(),
        reader: Box::new(std::io::Cursor::new(typed.unwrap_or("").as_bytes())),
    }
}

async fn adopt_otel(
    paths: &ShepPaths,
    binary: &Path,
    yes: bool,
    mut input: Input,
) -> (ExitCode, String) {
    let args = AdoptArgs {
        yes,
        name: Some("otel".to_string()),
        path: binary.to_path_buf(),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = adopt_asking(&mut streams(&mut out, &mut err), paths, &args, &mut input).await;
    (code, String::from_utf8(err).unwrap())
}

async fn enable_otel(paths: &ShepPaths, yes: bool, mut input: Input) -> (ExitCode, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = enable_asking(
        &mut streams(&mut out, &mut err),
        paths,
        "otel",
        yes,
        &mut input,
    )
    .await;
    (code, String::from_utf8(err).unwrap())
}

/// The grants `shep.toml` records for `otel`, as the names they list, and
/// whether `otel` is enabled.
fn recorded(paths: &ShepPaths) -> (Option<Vec<String>>, bool) {
    let cfg = ShepToml::read_only(&paths.daemon_config).unwrap();
    let granted = cfg
        .granted("otel")
        .unwrap()
        .map(|grants| grants.names().map(str::to_owned).collect());
    let enabled = cfg.enabled_dog_names().iter().any(|dog| dog == "otel");
    (granted, enabled)
}

fn adopted(paths: &ShepPaths) -> bool {
    ShepToml::read_only(&paths.daemon_config)
        .unwrap()
        .adopted_dog_path("otel")
        .is_some()
}

#[tokio::test]
async fn answering_no_leaves_the_dog_adopted_and_disabled_with_nothing_granted() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    let (_, asked) = adopt_otel(&paths, &binary, false, typing(Some("n\n"))).await;

    assert!(asked.contains("flock.read"), "{asked}");
    assert!(adopted(&paths));
    assert_eq!(recorded(&paths), (None, false));
}

#[tokio::test]
async fn answering_yes_grants_the_declared_list_and_enables_the_dog() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    let (code, _) = adopt_otel(&paths, &binary, false, typing(Some("y\n"))).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(
        recorded(&paths),
        (
            Some(vec![
                "events:process.".to_string(),
                "flock.read".to_string()
            ]),
            true
        )
    );
}

#[tokio::test]
async fn yes_grants_without_asking_even_with_no_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    let (code, _) = adopt_otel(&paths, &binary, true, typing(None)).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(recorded(&paths).0.map(|names| names.len()), Some(2));
}

#[tokio::test]
async fn no_terminal_and_no_yes_refuses_and_adopts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    let (code, err) = adopt_otel(&paths, &binary, false, typing(None)).await;

    assert_eq!(code, ExitCode::Usage);
    assert!(err.contains("--yes"), "{err}");
    assert!(!paths.daemon_config.exists(), "a refusal writes nothing");
}

#[tokio::test]
async fn an_unknown_required_permission_refuses_the_adopt() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(
        dir.path(),
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"telepathy","required":true,"reason":"reads minds"}]}"#,
    );

    let (code, err) = adopt_otel(&paths, &binary, true, typing(None)).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(err.contains("telepathy"), "{err}");
    assert!(!paths.daemon_config.exists());
}

#[tokio::test]
async fn an_unknown_optional_permission_is_dropped_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(
        dir.path(),
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"logs.read","required":true,"reason":"tails logs"},{"name":"telepathy","required":false,"reason":"reads minds"}]}"#,
    );

    let (code, err) = adopt_otel(&paths, &binary, true, typing(None)).await;

    assert_eq!(code, ExitCode::Success);
    assert!(err.contains("telepathy"), "{err}");
    assert_eq!(
        recorded(&paths),
        (Some(vec!["logs.read".to_string()]), true)
    );
}

#[tokio::test]
async fn an_undeclared_dog_is_asked_about_full_access_and_records_no_grants() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), "");

    let (code, asked) = adopt_otel(&paths, &binary, false, typing(Some("y\n"))).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        asked.contains("declares nothing, runs with full access"),
        "{asked}"
    );
    assert_eq!(recorded(&paths), (None, true));
}

/// A declined adopt, then the enable that asks again.
#[tokio::test]
async fn the_first_enable_of_a_declined_dog_asks_and_a_yes_grants_and_enables() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);
    adopt_otel(&paths, &binary, false, typing(Some("n\n"))).await;

    let (code, asked) = enable_otel(&paths, false, typing(Some("y\n"))).await;

    assert_eq!(code, ExitCode::Success);
    assert!(asked.contains("flock.read"), "{asked}");
    assert!(recorded(&paths).1);
    assert_eq!(recorded(&paths).0.map(|names| names.len()), Some(2));
}

#[tokio::test]
async fn declining_the_first_enable_leaves_the_dog_disabled_with_nothing_granted() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);
    adopt_otel(&paths, &binary, false, typing(Some("n\n"))).await;

    enable_otel(&paths, false, typing(Some("n\n"))).await;

    assert_eq!(recorded(&paths), (None, false));
}

/// Once granted, a later enable (after a disable) does not ask again.
#[tokio::test]
async fn enabling_a_dog_that_was_already_granted_does_not_ask() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);
    adopt_otel(&paths, &binary, true, typing(None)).await;
    ShepToml::edit(&paths.daemon_config, |doc| doc.disable_dog("otel")).unwrap();

    let (code, err) = enable_otel(&paths, false, typing(None)).await;

    assert_eq!(code, ExitCode::Success);
    assert!(!err.contains("[y/N]"), "{err}");
    assert!(recorded(&paths).1);
}

/// A reader that, as the operator is asked, has another shep write `otel`'s
/// grants into `shep.toml`, then types `answer`.
struct GrantedWhileAsking {
    config: PathBuf,
    answer: &'static [u8],
    asked: bool,
}

impl std::io::Read for GrantedWhileAsking {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let line = self.fill_buf()?.to_vec();
        let len = line.len().min(buf.len());
        buf[..len].copy_from_slice(&line[..len]);
        self.consume(len);
        Ok(len)
    }
}

impl BufRead for GrantedWhileAsking {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if !self.asked {
            self.asked = true;
            ShepToml::edit(&self.config, |doc| {
                doc.set_granted("otel", &Grants::from_names(["logs.read".to_string()]))
                    .unwrap();
            })
            .unwrap();
            return Ok(self.answer);
        }
        Ok(b"")
    }

    fn consume(&mut self, _len: usize) {
        // One answer, taken whole.
        self.asked = true;
    }
}

fn granted_while_asking(paths: &ShepPaths, answer: &'static [u8]) -> Input {
    Input {
        is_terminal: true,
        reader: Box::new(GrantedWhileAsking {
            config: paths.daemon_config.clone(),
            answer,
            asked: false,
        }),
    }
}

/// The grants another shep wrote while the question was open were decided by
/// someone else after the read that chose to ask: they stand.
#[tokio::test]
async fn a_grant_written_while_adopt_is_asking_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    adopt_otel(&paths, &binary, false, granted_while_asking(&paths, b"y\n")).await;

    assert_eq!(recorded(&paths).0, Some(vec!["logs.read".to_string()]));
}

#[tokio::test]
async fn a_grant_written_while_enable_is_asking_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);
    adopt_otel(&paths, &binary, false, typing(Some("n\n"))).await;

    enable_otel(&paths, false, granted_while_asking(&paths, b"y\n")).await;

    assert_eq!(
        recorded(&paths),
        (Some(vec!["logs.read".to_string()]), true)
    );
}

/// A no that lands after another shep granted the dog must not switch off a
/// dog that now holds grants.
#[tokio::test]
async fn a_no_after_a_concurrent_grant_does_not_disable_the_dog() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), DECLARES_TWO);

    adopt_otel(&paths, &binary, false, granted_while_asking(&paths, b"n\n")).await;

    assert_eq!(
        recorded(&paths),
        (Some(vec!["logs.read".to_string()]), true)
    );
}
