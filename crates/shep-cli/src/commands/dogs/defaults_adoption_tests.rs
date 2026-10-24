//! `shep adopt` writing a manifest's `defaults` into `dogs.toml`, through
//! the verb itself with a real `/bin/sh` dog. What an operator sees is
//! `dogs.toml` on disk: the section written, a section left alone, or no
//! file at all.

use std::path::{Path, PathBuf};

use shep_core::paths::ShepPaths;

use super::adopt::adopt;
use super::test_support::streams;
use crate::cli::AdoptArgs;
use crate::exit::ExitCode;

/// Writes a dog that answers `--shep-manifest` with `manifest` and exits 0
/// on any other run.
fn write_dog(dir: &Path, manifest: &str) -> PathBuf {
    let binary = dir.join("shep-otel");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n",
    );
    std::fs::write(&binary, script).unwrap();
    let mut mode = std::fs::metadata(&binary).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    std::fs::set_permissions(&binary, mode).unwrap();
    binary
}

/// Adopts `binary` as `otel` under `paths`, returning the exit code,
/// stderr, and the whole stdout the verb wrote.
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

/// The manifest of a dog declaring `interval = "1h"` as its one setting.
fn manifest_with_defaults(defaults: &str) -> String {
    let escaped = defaults
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!(
        r#"{{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"defaults":"{escaped}"}}"#
    )
}

#[tokio::test]
async fn adopting_a_dog_with_defaults_writes_its_section_to_dogs_toml() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    let written = std::fs::read_to_string(&paths.dogs_config).unwrap();
    assert_eq!(written, "[otel]\ninterval = \"1h\"\n");
}

/// The operator-facing half of #356: the adopt says where the knobs landed.
#[tokio::test]
async fn the_adopt_says_the_defaults_became_the_dogs_section() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert!(
        notices.contains("`[otel]` section in dogs.toml"),
        "{notices}"
    );
}

/// Exact bytes and inode: `adopt` must not rewrite a `dogs.toml` that
/// already has the section, and a rewrite that reproduces the bytes would
/// still strike the operator's file with a new inode and mode.
#[cfg(unix)]
#[tokio::test]
async fn adopting_over_an_existing_section_leaves_it_byte_for_byte() {
    use std::os::unix::fs::MetadataExt as _;

    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let before = "# hand-written, do not clobber\n[otel]\ninterval = \"5m\" # my own cadence\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let inode = std::fs::metadata(&paths.dogs_config).unwrap().ino();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert_eq!(std::fs::read_to_string(&paths.dogs_config).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&paths.dogs_config).unwrap().ino(),
        inode,
        "a section that is kept must not be rewritten at all"
    );
}

/// A second `adopt` of the same binary reads the cached manifest, so this
/// is the re-adopt an operator actually runs: the first adopt wrote the
/// section, the second must leave it alone.
#[tokio::test]
async fn adopting_a_second_time_does_not_write_the_section_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));
    adopt_otel(&paths, &binary).await;
    let after_first = std::fs::read_to_string(&paths.dogs_config).unwrap();

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).unwrap(),
        after_first
    );
}

/// A manifest that declares no `defaults`, and a dog with no manifest at
/// all, are the two ways a dog arrives with nothing to write: no
/// `dogs.toml` appears, and the adopt says nothing about defaults.
#[tokio::test]
async fn a_dog_with_no_defaults_writes_nothing() {
    for manifest in [
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12}"#,
        manifest_with_defaults("").as_str(),
        "not json at all",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let binary = write_dog(dir.path(), manifest);

        let (code, notices) = adopt_otel(&paths, &binary).await;

        assert_eq!(code, ExitCode::Success, "{manifest:?}: {notices}");
        assert!(
            !paths.dogs_config.exists(),
            "{manifest:?}: nothing was declared, so there is no file"
        );
        assert!(!notices.contains("defaults"), "{notices}");
    }
}

/// The dog's bug, told and survived: the adopt proceeds, the notice names
/// the broken `defaults`, and no `dogs.toml` is written from text shep
/// could not parse.
#[tokio::test]
async fn defaults_that_are_not_valid_toml_warn_and_the_adopt_proceeds() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert!(
        notices.contains("whose `defaults` is not valid TOML"),
        "{notices}"
    );
    assert!(!paths.dogs_config.exists());
}

/// A `defaults` body is the section's contents, so its own headers name
/// the dog's sub-tables: `[sinks]` arrives as `[otel.sinks]`, in one
/// piece after the section header, not interleaved with anything else in
/// the file.
#[tokio::test]
async fn defaults_with_their_own_headers_arrive_as_the_dogs_sub_tables() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let binary = write_dog(
        dir.path(),
        &manifest_with_defaults("interval = \"1h\"\n\n[sinks]\noncall = { url = \"u\" }\n"),
    );

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    let written = std::fs::read_to_string(&paths.dogs_config).unwrap();
    assert_eq!(
        written,
        "[otel]\ninterval = \"1h\"\n\n[otel.sinks]\noncall = { url = \"u\" }\n"
    );
}

/// The defaults are appended, and what the operator already wrote is not
/// re-rendered: comments, inline tables and the other dogs' sections
/// survive in their own bytes, and `[otel]` lands last.
#[tokio::test]
async fn defaults_land_after_the_sections_the_operator_already_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let before = "# hand-edited\n[bark.sinks]\noncall = { url = \"https://example.invalid/abc\" }\n\n[[bark.rules]]\non = \"gave_up\"\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).unwrap(),
        format!("{before}\n[otel]\ninterval = \"1h\"\n")
    );
}

/// A `dogs.toml` shep cannot read is a file it cannot write into: the
/// adopt refuses, naming the broken file, and the operator's bytes and the
/// registration both survive for a fix and a retry.
#[tokio::test]
async fn a_broken_dogs_toml_refuses_an_adopt_that_would_write_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let before = "[otel\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::InvalidConfig, "{notices}");
    assert!(notices.contains("dogs.toml"), "{notices}");
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).unwrap(),
        before,
        "a refused write strikes nothing"
    );
}

/// The broken file is judged before the dog's text: a `defaults` that is
/// also invalid must not let an unreadable `dogs.toml` ride through the
/// adopt hidden behind the warning about the dog.
#[tokio::test]
async fn a_broken_dogs_toml_refuses_even_when_the_defaults_are_also_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let before = "[otel\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::InvalidConfig, "{notices}");
    assert!(notices.contains("dogs.toml"), "{notices}");
    assert_eq!(
        std::fs::read_to_string(&paths.dogs_config).unwrap(),
        before,
        "a refused write strikes nothing"
    );
}

/// The warning about a dog's broken `defaults` says nothing was added, not
/// that the dog has no section: a section the operator wrote earlier is
/// still there, and the notice must not read as if it were missing.
#[tokio::test]
async fn the_unreadable_defaults_warning_says_nothing_was_added_not_that_no_section_exists() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    let before = "[otel]\ninterval = \"5m\" # mine\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    assert!(
        notices.contains("nothing was added to dogs.toml for it"),
        "{notices}"
    );
    assert!(
        !notices.contains("its section was not written"),
        "the notice may not claim a section is absent when one is there: {notices}"
    );
    assert_eq!(std::fs::read_to_string(&paths.dogs_config).unwrap(), before);
}

/// A home upgraded from the `[dog.<name>]` layout, re-adopted before its
/// first daemon boot: the legacy settings must move into `dogs.toml`
/// before the manifest's `defaults` are judged. Seeding first would write
/// a second copy of the section, and the boot's migration would then
/// refuse the name for holding values in both files.
#[tokio::test]
async fn re_adopting_a_legacy_dog_moves_its_settings_before_writing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.daemon_config, "[dog.otel]\ninterval = \"5m\"\n").unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::Success, "{notices}");
    let dogs = std::fs::read_to_string(&paths.dogs_config).unwrap();
    assert!(dogs.contains("interval = \"5m\""), "{dogs}");
    assert!(
        !dogs.contains("\"1h\""),
        "the manifest defaults must not second-guess the operator's settings: {dogs}"
    );
    assert!(
        notices.contains("moved dog config out of shep.toml and into dogs.toml: otel"),
        "the move must be reported, the way `muster` reports it: {notices}"
    );
    let shep_toml = std::fs::read_to_string(&paths.daemon_config).unwrap();
    assert!(!shep_toml.contains("[dog.otel]"), "{shep_toml}");
    let moved = crate::commands::dog_migration::migrate_dog_sections(&paths).unwrap();
    assert!(
        moved.is_empty(),
        "the next boot must find nothing left to move and nothing to refuse: {moved:?}"
    );
}

/// The same home with the collision already hand-made: values for the dog
/// in both files. `adopt` must refuse with the migration's own complaint
/// rather than succeed and leave the next boot to fail.
#[tokio::test]
async fn a_legacy_home_that_would_collide_refuses_the_adopt_before_boot_would() {
    let dir = tempfile::tempdir().unwrap();
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    std::fs::create_dir_all(&paths.home).unwrap();
    std::fs::write(&paths.daemon_config, "[dog.otel]\ninterval = \"5m\"\n").unwrap();
    let before = "[otel]\ninterval = \"10m\"\n";
    std::fs::write(&paths.dogs_config, before).unwrap();
    let binary = write_dog(dir.path(), &manifest_with_defaults("interval = \"1h\"\n"));

    let (code, notices) = adopt_otel(&paths, &binary).await;

    assert_eq!(code, ExitCode::InvalidConfig, "{notices}");
    assert!(notices.contains("otel"), "{notices}");
    assert_eq!(std::fs::read_to_string(&paths.dogs_config).unwrap(), before);
}
