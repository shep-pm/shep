//! `shep dogs --available` against a real index server, including what it
//! must never print.

use super::*;

/// Rex's description carries a raw `\u{1b}[2J` screen-clear escape. The
/// assertion is on raw stdout bytes, so a regression cannot hide behind
/// `String::from_utf8_lossy`'s replacement character.
#[test]
fn available_dogs_lists_the_index_and_never_leaks_a_raw_escape() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert_success(&output);

    assert!(
        !output.stdout.contains(&0x1b),
        "a raw escape reached stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "NAME",
        "PACKAGE",
        "CATEGORY",
        "DESCRIPTION",
        "Spot",
        "shep-log-rotate",
        "logs",
        "Rex",
        "shep-watchdog",
        "health",
    ] {
        assert!(
            stdout.contains(expected),
            "table is missing {expected:?}: {stdout}"
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1 entry contained control characters"),
        "stderr must note the sanitised entry: {stderr}"
    );
}

/// A dog cannot learn the name it was adopted under, so a wrong name here
/// ships a copy-pasteable command that discards its whole config section.
#[test]
fn available_dogs_detail_view_uses_adopt_as_never_name() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .arg("spot")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Spot . shep-log-rotate . logs"),
        "detail header line: {stdout}"
    );
    assert!(
        stdout.contains("$ cargo install --git https://github.com/shep-pm/shep-log-rotate"),
        "install command: {stdout}"
    );
    assert!(
        stdout.contains("$ shep adopt ~/.cargo/bin/shep-log-rotate --name log-rotate"),
        "adopt command must use adopt_as (log-rotate), not name (Spot): {stdout}"
    );
    assert!(
        !stdout.contains("--name Spot"),
        "adopt command must never use the display name: {stdout}"
    );
}

#[test]
fn available_dogs_zero_matches_exits_zero_and_says_so() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .arg("wombat")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no dog matches \"wombat\""),
        "stdout: {stdout}"
    );
}

/// Neither a socket nor a pidfile may exist afterwards, so an autostart is
/// caught even when the command still answers successfully.
#[test]
fn available_dogs_needs_no_shepherd() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(
        !home.path().join("run").join("shep.sock").exists(),
        "--available must never bring up a shepherd"
    );
    assert!(
        !home.path().join("pids").join("shepd.pid").exists(),
        "--available must never bring up a shepherd"
    );
}

/// `IndexError` carries the URL on no variant but `InsecureUrl`, so
/// `available_dogs` is what names it.
#[test]
fn available_dogs_reports_a_server_error_naming_the_url() {
    let home = TempDir::new().unwrap();
    let url = serve_raw_response(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
    );

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a 500 must not exit success: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("reading the dog index from {url}")),
        "stderr must name the failing url: {stderr}"
    );
    assert!(stderr.contains("500"), "stderr: {stderr}");
}

/// The one url `available_dogs` does not name. `SHEP_DOG_INDEX` is an
/// operator's own string, so a password can reach it, and this message is
/// built outside `fetch` and outside `IndexError` where neither refusal
/// covers it.
#[test]
fn available_dogs_names_no_url_that_carries_credentials() {
    // A sentinel per component, none of them a substring of anything the
    // message says on its own. A password redacted while the username or
    // the host it was paired with still prints is a narrower leak, not a
    // closed one.
    for url in [
        "ftp://sentineluser:hunter2@sentinelhost.invalid/dogs.json",
        // Scheme-relative, so there is no `://` to split the authority on.
        "//sentineluser:hunter2@sentinelhost.invalid/dogs.json",
        // The `@` is in a path here, so the authority predicate says no
        // and only the blunt printing rule stands between this and
        // stderr. `parse_url` withheld this url while the sentence around
        // it printed the same one, until both asked the same question.
        "file:///etc/sentineluser:hunter2@sentinelhost.invalid",
    ] {
        let home = TempDir::new().unwrap();

        let output = shep(home.path())
            .env("SHEP_DOG_INDEX", url)
            .arg("dogs")
            .arg("--available")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{url}: an unfetchable url must not exit success: {output:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        for secret in ["hunter2", "sentineluser", "sentinelhost.invalid"] {
            assert!(
                !stderr.contains(secret),
                "{url}: stderr printed {secret}: {stderr}"
            );
        }
        assert!(
            stderr.contains("a url that may carry credentials"),
            "{url}: stderr must say why it withheld the url: {stderr}"
        );
    }
}

#[test]
fn available_dogs_reports_a_truncated_body_naming_the_url() {
    let home = TempDir::new().unwrap();
    // Declares 100 bytes of body, sends 2, then closes: `fetch::get`'s
    // `Truncated` refusal.
    let url = serve_raw_response("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n[]".to_string());

    let output = shep(home.path())
        .env("SHEP_DOG_INDEX", &url)
        .arg("dogs")
        .arg("--available")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a truncated body must not exit success: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("reading the dog index from {url}")),
        "stderr must name the failing url: {stderr}"
    );
    assert!(stderr.contains("truncated"), "stderr: {stderr}");
}
