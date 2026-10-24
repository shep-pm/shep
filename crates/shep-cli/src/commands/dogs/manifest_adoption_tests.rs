//! `shep adopt` and `shep enable` learning a dog from its manifest, through
//! the verbs themselves with real `/bin/sh` dogs. What an operator sees is
//! `shep.toml`, stderr, and how many times the binary was run.

use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;

use super::adopt::adopt;
use super::enable::enable;
use super::test_support::streams;
use crate::cli::AdoptArgs;
use crate::exit::ExitCode;

/// A dog's `--shep-manifest` answer: `manifest` is printed verbatim, and
/// every run of the flag appends a byte to the returned counter file.
///
/// `extra` is more shell, run for any other flag, such as an answer to
/// `--version`.
fn write_dog(dir: &Path, manifest: &str, extra: &str) -> (PathBuf, PathBuf) {
    let binary = dir.join("shep-otel");
    let count = dir.join("manifest-runs");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then printf x >> '{count}'; cat <<'EOF'\n\
         {manifest}\nEOF\nexit 0\nfi\n{extra}\nexit 0\n",
        count = count.display(),
    );
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    (binary, count)
}

fn runs(count: &Path) -> usize {
    std::fs::read_to_string(count).map_or(0, |text| text.len())
}

/// Adopts `binary` as `otel` under `paths`, returning the exit code and
/// stderr.
async fn adopt_otel(paths: &ShepPaths, binary: &Path) -> (ExitCode, String) {
    let args = AdoptArgs {
        yes: true,
        name: Some("otel".to_string()),
        path: binary.to_path_buf(),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = adopt(&mut streams(&mut out, &mut err), paths, &args).await;
    (code, String::from_utf8(err).unwrap())
}

const MANIFEST: &str = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12}"#;

#[tokio::test]
async fn adopting_a_manifest_dog_reads_it_once_and_adopting_it_again_reads_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (binary, count) = write_dog(dir.path(), MANIFEST, "");

    let (first, _) = adopt_otel(&paths, &binary).await;
    assert_eq!(first, ExitCode::Success);
    assert_eq!(runs(&count), 1, "the first adopt asks the dog");

    let (second, _) = adopt_otel(&paths, &binary).await;
    assert_eq!(second, ExitCode::Success);
    assert_eq!(runs(&count), 1, "the same binary is read from the cache");
}

#[tokio::test]
async fn rebuilding_the_binary_probes_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (binary, count) = write_dog(dir.path(), MANIFEST, "");
    adopt_otel(&paths, &binary).await;

    let rebuilt = MANIFEST.replace("0.2.0", "0.3.0");
    write_dog(dir.path(), &rebuilt, "");
    let (code, _) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(runs(&count), 2, "a changed binary is asked again");
}

/// What a dog that never heard of `--shep-manifest` gets: today's flags.
#[tokio::test]
async fn a_dog_with_no_usable_manifest_adopts_through_the_old_flags() {
    let version =
        "if [ \"$1\" = --version ]; then printf 'shep-otel 0.1.0\\nshep-channel: true\\n'; fi";
    for manifest in ["", "not json at all", r#"{"name":"otel"}"#] {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let (binary, count) = write_dog(dir.path(), manifest, version);

        let (code, notices) = adopt_otel(&paths, &binary).await;

        assert_eq!(code, ExitCode::Success, "{manifest:?}: {notices}");
        assert!(notices.contains("reports version 0.1.0"), "{notices}");
        let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
        assert!(written.contains("channel_dogs"), "{manifest:?}: {written}");
        adopt_otel(&paths, &binary).await;
        assert_eq!(
            runs(&count),
            2,
            "an answer that is no manifest is not cached"
        );
    }
}

#[tokio::test]
async fn a_dog_that_fails_the_manifest_flag_adopts_through_the_old_flags() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = dir.path().join("shep-otel");
    let script = "#!/bin/sh\ncase \"$1\" in\n--shep-manifest) echo '{\"manifest\":1,\"name\":\"otel\",\"version\":\"9\",\"protocol\":12}'; exit 3;;\n--version) printf 'shep-otel 0.1.0\\n';;\nesac\nexit 0\n";
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert!(notices.contains("reports version 0.1.0"), "{notices}");
}

/// The dog answers nothing to `--version` here, so everything shep learns
/// of it comes from the manifest.
#[tokio::test]
async fn the_manifest_supplies_the_version_and_the_channel_ask() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let manifest = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"channel":true}"#;
    let (binary, _) = write_dog(dir.path(), manifest, "");

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        notices.contains("reports version 0.2.0, shep protocol 12"),
        "{notices}"
    );
    let written = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(written.contains("channel_dogs"), "{written}");
}

#[tokio::test]
async fn a_manifest_protocol_below_the_floor_refuses_the_adopt() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let manifest = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":1}"#;
    let (binary, _) = write_dog(dir.path(), manifest, "");

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(notices.contains("built for shep protocol 1"), "{notices}");
    assert!(!paths.daemon_config.exists());
}

fn manifest_depending_on(depends: &str) -> String {
    format!(r#"{{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"depends":{depends}}}"#)
}

const NEEDS_LOG_ROTATE: &str = r#"{"dogs":[{"name":"log-rotate","version":">=0.1"}]}"#;

#[tokio::test]
async fn adopt_warns_of_a_dependency_that_is_not_adopted_and_still_adopts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (binary, _) = write_dog(dir.path(), &manifest_depending_on(NEEDS_LOG_ROTATE), "");

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        notices.contains("`otel` depends on `log-rotate`, which is not adopted"),
        "{notices}"
    );
    assert!(
        std::fs::read_to_string(&paths.daemon_config)
            .unwrap()
            .contains("otel")
    );
}

#[tokio::test]
async fn adopt_stays_quiet_about_a_dependency_that_is_adopted_or_built_in() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (rotate, _) = write_dog(dir.path(), "", "");
    let rotate = {
        let renamed = dir.path().join("shep-log-rotate");
        std::fs::rename(rotate, &renamed).unwrap();
        renamed
    };
    let args = AdoptArgs {
        yes: true,
        name: Some("log-rotate".to_string()),
        path: rotate,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    adopt(&mut streams(&mut out, &mut err), &paths, &args).await;
    let depends =
        r#"{"dogs":[{"name":"log-rotate","version":">=0.1"},{"name":"metrics","version":"*"}]}"#;
    let (binary, _) = write_dog(dir.path(), &manifest_depending_on(depends), "");

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert!(!notices.contains("depends on"), "{notices}");
}

#[tokio::test]
async fn adopt_warns_of_a_dog_that_needs_a_newer_shep_and_still_adopts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (binary, _) = write_dog(
        dir.path(),
        &manifest_depending_on(r#"{"shep":"999.0.0"}"#),
        "",
    );

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success);
    assert!(
        notices.contains(&format!(
            "`otel` needs shep 999.0.0 or newer, and this is shep {}",
            env!("CARGO_PKG_VERSION")
        )),
        "{notices}"
    );
    assert!(
        std::fs::read_to_string(&paths.daemon_config)
            .unwrap()
            .contains("otel")
    );
}

#[tokio::test]
async fn adopt_stays_quiet_when_this_shep_is_new_enough() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (binary, _) = write_dog(
        dir.path(),
        &manifest_depending_on(r#"{"shep":"0.1.0"}"#),
        "",
    );

    let (_, notices) = adopt_otel(&paths, &binary).await;

    assert!(!notices.contains("needs shep"), "{notices}");
}

#[tokio::test]
async fn enable_of_an_adopted_dog_gives_both_warnings_and_still_enables() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let depends = r#"{"dogs":[{"name":"log-rotate","version":">=0.1"}],"shep":"999.0.0"}"#;
    let (binary, _) = write_dog(dir.path(), &manifest_depending_on(depends), "");
    adopt_otel(&paths, &binary).await;

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = enable(&mut streams(&mut out, &mut err), &paths, "otel", true).await;

    assert_eq!(code, ExitCode::Success);
    let notices = String::from_utf8(err).unwrap();
    assert!(notices.contains("depends on `log-rotate`"), "{notices}");
    assert!(notices.contains("needs shep 999.0.0 or newer"), "{notices}");
}

#[tokio::test]
async fn enable_of_a_built_in_dog_says_nothing_of_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let mut out = Vec::new();
    let mut err = Vec::new();

    let code = enable(&mut streams(&mut out, &mut err), &paths, "metrics", true).await;

    assert_eq!(code, ExitCode::Success);
    assert_eq!(String::from_utf8(err).unwrap(), "");
}
