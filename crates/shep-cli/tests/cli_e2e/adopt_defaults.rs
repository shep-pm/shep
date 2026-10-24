//! `shep adopt` writing a manifest dog's `defaults` into `dogs.toml`,
//! through the real binary: the section an operator finds on disk, and the
//! file that is not created for a dog that declared nothing.

use super::*;

/// A dog answering `--shep-manifest` with `manifest` and nothing else.
fn write_manifest_dog(dir: &TempDir, manifest: &str) -> PathBuf {
    write_script(
        dir,
        "shep-otel.sh",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then printf '%s' '{manifest}'; fi\nexit 0\n",
        ),
    )
}

fn adopt(home: &TempDir, binary: &Path) -> Output {
    Command::cargo_bin("shep")
        .unwrap()
        .arg("--home")
        .arg(home.path())
        .arg("adopt")
        .arg("--yes")
        .arg(binary)
        .arg("--name")
        .arg("otel")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap()
}

/// The `defaults` of a manifest is a JSON string, so its TOML arrives
/// escaped.
fn manifest_with_defaults(defaults: &str) -> String {
    let escaped = defaults
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!(
        r#"{{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"defaults":"{escaped}"}}"#
    )
}

#[cfg(unix)]
#[test]
fn shep_adopt_writes_a_manifest_dogs_defaults_as_its_dogs_toml_section() {
    let home = TempDir::new().unwrap();
    let binary = write_manifest_dog(&home, &manifest_with_defaults("interval = \"1h\"\n"));

    let output = adopt(&home, &binary);
    assert_success(&output);

    let written = std::fs::read_to_string(home.path().join("dogs.toml")).unwrap();
    assert_eq!(written, "[otel]\ninterval = \"1h\"\n");

    // Re-adopting is what an operator actually runs after an upgrade: the
    // section that is there is not touched a second time.
    let output = adopt(&home, &binary);
    assert_success(&output);
    assert_eq!(
        std::fs::read_to_string(home.path().join("dogs.toml")).unwrap(),
        "[otel]\ninterval = \"1h\"\n"
    );
}

#[cfg(unix)]
#[test]
fn shep_adopt_creates_no_dogs_toml_for_a_dog_without_defaults() {
    for manifest in [
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12}"#,
        "",
    ] {
        let home = TempDir::new().unwrap();
        let binary = write_manifest_dog(&home, manifest);

        let output = adopt(&home, &binary);
        assert_success(&output);

        assert!(
            !home.path().join("dogs.toml").exists(),
            "{manifest:?}: nothing declared means no file"
        );
    }
}

/// The exact notice lines the `writing-a-dog` docs quote, pinned through
/// the real binary: the dog name prints bare, and a docs sample that
/// decorates it is a string nobody's stderr will match.
#[cfg(unix)]
#[test]
fn shep_adopt_prints_the_defaults_notices_the_docs_quote() {
    let home = TempDir::new().unwrap();
    let binary = write_manifest_dog(&home, &manifest_with_defaults("interval = \"1h\"\n"));

    let output = adopt(&home, &binary);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "notice[dog_defaults_written]: otel's manifest defaults are now its \
             `[otel]` section in dogs.toml, where you can edit them"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let home = TempDir::new().unwrap();
    let binary = write_manifest_dog(&home, &manifest_with_defaults("interval = \n"));

    let output = adopt(&home, &binary);
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "notice[dog_defaults_unreadable]: otel shipped a manifest whose `defaults` \
             is not valid TOML, so nothing was added to dogs.toml for it. The dog is \
             adopted and runs normally; this is a bug to report to whoever wrote it"
        ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
