//! `shep adopt` and the first `shep enable` of a dog asking for the
//! permissions it declares, through the real binary: the prompt an operator
//! reads, what `--yes` grants, and what lands in `shep.toml`.

use super::*;

/// A dog answering `--shep-manifest` with `manifest`, or with nothing at all.
fn write_dog(dir: &TempDir, manifest: &str) -> PathBuf {
    write_script(
        dir,
        "shep-otel.sh",
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then printf '%s' '{manifest}'; fi\nexit 0\n",
        ),
    )
}

const DECLARES_TWO: &str = r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"},{"name":"events:process.","required":false,"reason":"hears when a sheep exits"}]}"#;

fn shep_toml(home: &TempDir) -> String {
    std::fs::read_to_string(home.path().join("shep.toml")).unwrap_or_default()
}

fn piped(home: &TempDir, args: &[&str]) -> Output {
    shep(home.path()).args(args).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
#[test]
fn yes_grants_the_declared_list_and_nothing_else() {
    let home = TempDir::new().unwrap();
    let binary = write_dog(&home, DECLARES_TWO);

    let output = piped(
        &home,
        &["adopt", "--yes", binary.to_str().unwrap(), "--name", "otel"],
    );

    assert_success(&output);
    let written = shep_toml(&home);
    assert!(
        written
            .contains("[daemon.dog_grants.otel]\ngranted = [\"events:process.\", \"flock.read\"]"),
        "{written}"
    );
    assert!(written.contains("enabled_dogs = [\"otel\"]"), "{written}");
}

#[cfg(unix)]
#[test]
fn no_terminal_and_no_yes_refuses_and_records_nothing() {
    let home = TempDir::new().unwrap();
    let binary = write_dog(&home, DECLARES_TWO);

    let output = piped(
        &home,
        &["adopt", binary.to_str().unwrap(), "--name", "otel"],
    );

    assert!(!output.status.success(), "{}", stderr(&output));
    let said = stderr(&output);
    assert!(
        said.contains("no terminal") && said.contains("--yes"),
        "{said}"
    );
    assert!(!shep_toml(&home).contains("otel"), "{}", shep_toml(&home));
}

#[cfg(unix)]
#[test]
fn an_unknown_required_permission_refuses_the_adopt() {
    let home = TempDir::new().unwrap();
    let binary = write_dog(
        &home,
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"telepathy","required":true,"reason":"reads minds"}]}"#,
    );

    let output = piped(
        &home,
        &["adopt", "--yes", binary.to_str().unwrap(), "--name", "otel"],
    );

    assert!(!output.status.success());
    assert!(stderr(&output).contains("telepathy"), "{}", stderr(&output));
    assert!(!shep_toml(&home).contains("otel"), "{}", shep_toml(&home));
}

#[cfg(unix)]
#[test]
fn an_unknown_optional_permission_is_dropped_with_a_warning() {
    let home = TempDir::new().unwrap();
    let binary = write_dog(
        &home,
        r#"{"manifest":1,"name":"otel","version":"0.2.0","protocol":12,"permissions":[{"name":"logs.read","required":true,"reason":"tails logs"},{"name":"telepathy","required":false,"reason":"reads minds"}]}"#,
    );

    let output = piped(
        &home,
        &["adopt", "--yes", binary.to_str().unwrap(), "--name", "otel"],
    );

    assert_success(&output);
    let said = stderr(&output);
    assert!(
        said.contains("telepathy") && said.contains("dropped"),
        "{said}"
    );
    let written = shep_toml(&home);
    assert!(written.contains("granted = [\"logs.read\"]"), "{written}");
    assert!(!written.contains("telepathy"), "{written}");
}

/// A dog that was adopted and never granted: the shape an adopt that was
/// answered no leaves behind.
#[cfg(unix)]
#[test]
fn enable_without_a_terminal_or_yes_refuses_a_dog_that_was_never_granted() {
    let home = TempDir::new().unwrap();
    let binary = write_dog(&home, DECLARES_TWO);
    std::fs::write(
        home.path().join("shep.toml"),
        format!("[daemon.adopted_dogs]\notel = \"{}\"\n", binary.display()),
    )
    .unwrap();

    let refused = piped(&home, &["enable", "otel"]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--yes"), "{}", stderr(&refused));
    assert!(!shep_toml(&home).contains("dog_grants"));

    assert_success(&piped(&home, &["enable", "--yes", "otel"]));
    assert!(shep_toml(&home).contains("granted = [\"events:process.\", \"flock.read\"]"));
}
