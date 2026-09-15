//! `shep import` and `shep import env`, across every way a dotenv can
//! disagree with what is already stored.

use super::*;

#[cfg(unix)]
/// The written file is parsed back through the real
/// `shep_core::config::Flockfile::parse`: a Flockfile shep refuses to read is
/// not an import. That no socket appears is the other half, since `import`
/// takes no `Client`.
#[test]
fn import_writes_a_flockfile_shep_can_read_back_and_starts_no_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let out = home.join("Flockfile.toml");
    let dump = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/commands/import/pm2/testdata/dump.pm2.json"
    );
    let mut guard = DaemonGuard::default();

    let output = shep(home)
        .arg("--format")
        .arg("json")
        .arg("import")
        .arg("pm2")
        .arg("--from")
        .arg(dump)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    guard.adopt_home(home);
    assert_success(&output);

    assert!(
        !home.join("run").join("shep.sock").exists(),
        "`shep import` reads a file and writes a file; it must never \
         autostart a daemon"
    );

    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["command"], "import",
        "`shep import` must reach the import verb and no other: {envelope}"
    );
    let rows = envelope["data"]
        .as_array()
        .unwrap_or_else(|| panic!("import data must be an array: {envelope}"));
    assert_eq!(rows.len(), 3, "{envelope}");

    let written = std::fs::read_to_string(&out).unwrap();
    let parsed =
        shep_core::config::Flockfile::parse(&written, shep_core::config::FlockFormat::Toml)
            .unwrap_or_else(|e| {
                panic!("shep import wrote a Flockfile shep cannot read back: {e}\n{written}")
            });
    assert_eq!(parsed.apps.len(), 3, "{written}");
}

/// A shepherd on `home`'s `$SHEP_HOME` with one sheep named `web`, which is
/// what every `shep import env` case below writes against: the verb records
/// an operator override, so the sheep has to be registered first.
#[cfg(unix)]
fn start_a_sheep_named_web(home: &TempDir) -> DaemonGuard {
    let script = write_test_script(home);
    let mut guard = DaemonGuard::default();
    let boot = shep(home.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&boot);
    guard
}

#[cfg(unix)]
/// The whole verb, end to end: two plain keys into the sheep's env, one
/// secret into the store with a reference left behind.
#[test]
fn import_env_splits_a_dotenv_between_the_two_stores() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nPORT=8080\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("hunter2") && !combined.contains("8080"),
        "a value reached an output stream: {combined}"
    );

    // `describe` reads its secret references out of the muster roll on disk,
    // which only a save writes: the import records an operator override, and
    // the daemon holds the parked spec in memory until something rolls it.
    assert_success(&shep(home.path()).arg("save").output().unwrap());

    // The roll is where the reference itself lands, so it is where the
    // literal token is asserted. `describe` reports the reference resolved
    // rather than reprinting it, and that is the second half of the claim:
    // a bare `contains("DB_PASSWORD")` would pass on an unresolved one.
    let rolled = std::fs::read_to_string(home.path().join("flock.json")).unwrap();
    assert!(
        rolled.contains("{{secret:DB_PASSWORD}}"),
        "the reference did not reach the sheep: {rolled}"
    );

    let described = shep(home.path())
        .args(["describe", "web", "--format", "json"])
        .output()
        .unwrap();
    let described: serde_json::Value =
        serde_json::from_slice(&described.stdout).expect("describe --format json emits JSON");
    assert_eq!(
        described["secrets"],
        serde_json::json!([{
            "name": "web",
            "reference": "DB_PASSWORD",
            "environment": "production",
            "status": "resolved",
        }]),
        "{described}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A `.env` value is a value, not a shep template.
///
/// The sheep's env is read as a template grammar, where `{{world}}` is
/// refused at config time and `{{name}}` substitutes the sheep's own name.
/// A `.env` promises neither, so the child has to receive both exactly as
/// the file wrote them. The reload is what promotes the parked env.
#[test]
fn import_env_hands_the_child_a_braced_value_exactly_as_the_file_wrote_it() {
    let home = tempfile::tempdir().unwrap();
    let script = write_script(
        &home,
        "braces.sh",
        &format!(
            "{}{}echo \"motd=[$MOTD]\"\necho \"greeting=[$GREETING]\"\n{}",
            script_header(),
            record_pid_line(&home),
            sleep_line(SCRIPT_SLEEP_SECS)
        ),
    );
    let mut guard = DaemonGuard::default();
    let boot = shep(home.path())
        .arg("start")
        .arg(&script)
        .arg("--name")
        .arg("web")
        .output()
        .unwrap();
    guard.adopt_home(home.path());
    assert_success(&boot);

    std::fs::write(
        home.path().join("app.env"),
        "MOTD=hello {{world}}\nGREETING={{name}}-prod\n",
    )
    .unwrap();
    let imported = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&imported);

    assert_success(&shep(home.path()).args(["reload", "web"]).output().unwrap());

    let bleats = bleats_no_follow_until_contains(
        home.path(),
        &["web"],
        &["motd=[hello {{world}}]", "greeting=[{{name}}-prod]"],
    );
    let printed = String::from_utf8_lossy(&bleats.stdout);
    assert!(
        printed.contains("motd=[hello {{world}}]"),
        "an unknown token must reach the child literally: {printed}"
    );
    assert!(
        printed.contains("greeting=[{{name}}-prod]"),
        "and a token shep does define must not be substituted: {printed}"
    );
    assert!(
        !printed.contains("greeting=[web-prod]"),
        "the sheep's name was substituted into a .env value: {printed}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// Re-running an unchanged file is a no-op. Changing one value refuses the
/// whole import until `--force`.
#[test]
fn import_env_refuses_a_changed_value_without_force() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "PORT=8080\nNODE_ENV=production\n",
    )
    .unwrap();
    let first = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&first);

    std::fs::write(
        home.path().join("app.env"),
        "PORT=9090\nNODE_ENV=production\n",
    )
    .unwrap();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("PORT"),
        "the colliding key was not named: {err}"
    );
    // The daemon compares against the sheep's intended config, so a key a
    // Flockfile declares collides without the override store holding it.
    // A line naming an "env store" sends the operator to a file the value
    // need not be in.
    assert!(
        err.contains("the sheep's env"),
        "the refusal must name what actually holds the key: {err}"
    );
    assert!(!err.contains("9090"), "the value reached stderr: {err}");
    // The refusal's whole claim is that nothing moved, and this is the only
    // case where a store could have been written before it: the two
    // neighbouring refusals are parse-level.
    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap();
    assert!(
        overrides.contains("8080") && !overrides.contains("9090"),
        "the refused value reached the env store: {overrides}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A key the secret store already holds under a different value refuses the
/// import too, and names the secret store rather than the env one.
///
/// `--env production` on both halves so the seeded slot and the imported one
/// are the same slot whatever the sheep resolves to.
#[test]
fn import_env_refuses_a_changed_secret_without_force() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    assert_success(
        &shep(home.path())
            .args([
                "secret",
                "set",
                "DB_PASSWORD",
                "correct",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );
    std::fs::write(home.path().join("app.env"), "DB_PASSWORD=hunter2\n").unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--env",
            "production",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("DB_PASSWORD") && err.contains("secret store"),
        "the secret arm did not report the collision: {err}"
    );
    assert!(!err.contains("hunter2"), "the value reached stderr: {err}");
    let stored = std::fs::read_to_string(home.path().join("secrets.json")).unwrap();
    assert!(
        stored.contains("correct") && !stored.contains("hunter2"),
        "the refused value reached the secret store"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// `--force` takes both stores over the values already in them.
///
/// `--format json` pins the envelope's `command`, which is `import` for both
/// halves of the verb: the noun names the command, as `shep secret`'s four
/// subcommands do.
#[test]
fn import_env_force_overwrites_both_stores() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    assert_success(
        &shep(home.path())
            .args([
                "secret",
                "set",
                "DB_PASSWORD",
                "stale",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );
    std::fs::write(home.path().join("app.env"), "PORT=8080\n").unwrap();
    assert_success(
        &shep(home.path())
            .args([
                "import",
                "env",
                home.path().join("app.env").to_str().unwrap(),
                "--app",
                "web",
                "--env",
                "production",
            ])
            .output()
            .unwrap(),
    );

    std::fs::write(
        home.path().join("app.env"),
        "PORT=9090\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();
    let forced = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--env",
            "production",
            "--force",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_success(&forced);
    let envelope: serde_json::Value = serde_json::from_slice(&forced.stdout).unwrap();
    assert_eq!(
        envelope["command"], "import",
        "the envelope's command moved: {envelope}"
    );

    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap();
    assert!(
        overrides.contains("9090") && !overrides.contains("8080"),
        "--force left the old env value: {overrides}"
    );
    let stored = std::fs::read_to_string(home.path().join("secrets.json")).unwrap();
    assert!(
        stored.contains("hunter2") && !stored.contains("stale"),
        "--force left the old secret"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A key that looks like a secret and was not named by `--secret` is warned
/// about, and the warning carries the key and not its value.
#[test]
fn import_env_warns_about_a_secretish_key_it_was_not_told_to_hide() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(home.path().join("app.env"), "STRIPE_TOKEN=hunter2\n").unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("STRIPE_TOKEN") && err.contains("--secret"),
        "the secretish warning did not appear: {err}"
    );
    assert!(!err.contains("hunter2"), "the value reached stderr: {err}");

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A pattern that matches nothing refuses before anything is written.
#[test]
fn import_env_refuses_a_pattern_that_matches_nothing() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "ABSENT_*",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        !home.path().join("secrets.json").exists(),
        "a refused pattern must write nothing"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// `--dry-run` writes to neither store.
#[test]
fn import_env_dry_run_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "PORT=8080\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    // Both stores, because the fixture's plain key never reaches the secret
    // one: a dry run that sent a non-dry batch would move `overrides.json`
    // alone and pass a check that only read `secrets.json`.
    let secrets_before = std::fs::read_to_string(home.path().join("secrets.json")).ok();
    let overrides_before = std::fs::read_to_string(home.path().join("overrides.json")).ok();
    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "DB_PASSWORD",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(home.path().join("secrets.json")).ok(),
        secrets_before
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("overrides.json")).ok(),
        overrides_before
    );
    // The dry run is the path that prints a row per key, so it is the one
    // where a value would show up if a row ever grew one.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("hunter2") && !combined.contains("8080"),
        "a value reached an output stream: {combined}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// A `.env` that names a variable shep injects itself is refused by the
/// dry-run probe at step 3, which runs `normalize` on the merged config, so
/// neither store is touched.
///
/// This used to reach the real send instead and leave an orphaned secret in
/// `secrets.json`. The refusal now lands before the secret write, and the
/// secret's value still reaches neither output stream.
#[test]
fn import_env_refuses_a_reserved_variable_before_either_store_is_written() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("bad.env"),
        "SHEP_NAME=nope\nAPI_TOKEN=sk_live_abcdef\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("bad.env").to_str().unwrap(),
            "--app",
            "web",
            "--secret",
            "API_TOKEN",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4), "{output:?}");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("sk_live_abcdef"),
        "the value reached an output stream: {combined}"
    );
    assert!(
        !combined.contains("already written to the secret store"),
        "nothing was written, so nothing may be disclosed: {combined}"
    );

    let secrets = std::fs::read_to_string(home.path().join("secrets.json")).unwrap_or_default();
    assert!(
        !secrets.contains("API_TOKEN"),
        "the secret store was written by a refused import: {secrets}"
    );
    let overrides = std::fs::read_to_string(home.path().join("overrides.json")).unwrap_or_default();
    assert!(
        !overrides.contains("API_TOKEN") && !overrides.contains("SHEP_NAME"),
        "the override store was written by a refused import: {overrides}"
    );

    graceful_kill(home.path());
}

#[cfg(unix)]
/// An unknown sheep is a `NotFound`, and it is reported before either store
/// is touched.
#[test]
fn import_env_refuses_an_unknown_app() {
    let home = tempfile::tempdir().unwrap();
    let _guard = start_a_sheep_named_web(&home);
    std::fs::write(
        home.path().join("app.env"),
        "NODE_ENV=production\nDB_PASSWORD=hunter2\n",
    )
    .unwrap();

    let output = shep(home.path())
        .args([
            "import",
            "env",
            home.path().join("app.env").to_str().unwrap(),
            "--app",
            "absent",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        !home.path().join("secrets.json").exists(),
        "an unknown sheep must be refused before either store is touched"
    );

    graceful_kill(home.path());
}
