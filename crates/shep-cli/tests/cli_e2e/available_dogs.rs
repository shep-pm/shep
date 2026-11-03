//! `shep dogs --available` against a real index server, including what it
//! must never print.

use super::*;

/// Runs `shep dogs --available` with `SHEP_DOG_INDEX` set to `url`, `args`
/// appended after the flag, and hands back the raw `Output`. Which cases
/// expect success and which expect a refusal is the caller's own assertion.
#[track_caller]
fn available_dogs(home: &Path, url: &str, args: &[&str]) -> Output {
    shep(home)
        .env("SHEP_DOG_INDEX", url)
        .arg("dogs")
        .arg("--available")
        .args(args)
        .output()
        .unwrap()
}

/// Rex's description carries a raw `\u{1b}[2J` screen-clear escape. The
/// assertion is on raw stdout bytes, so a regression cannot hide behind
/// `String::from_utf8_lossy`'s replacement character.
#[test]
fn available_dogs_lists_the_index_and_never_leaks_a_raw_escape() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());

    let output = available_dogs(home.path(), &url, &[]);
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

    let output = available_dogs(home.path(), &url, &["spot"]);
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

    let output = available_dogs(home.path(), &url, &["wombat"]);
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

    let output = available_dogs(home.path(), &url, &[]);
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

    let output = available_dogs(home.path(), &url, &[]);
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

        let output = available_dogs(home.path(), url, &[]);
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

    let output = available_dogs(home.path(), &url, &[]);
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

/// The server answers once and is gone, so a second run that lists anyway
/// is reading the cache the first one wrote.
#[test]
fn available_dogs_reads_a_fresh_cache_without_fetching_again() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());
    assert_success(&available_dogs(home.path(), &url, &[]));

    let output = available_dogs(home.path(), &url, &[]);

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("shep-watchdog"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cached"),
        "a fresh cache is not news: {stderr}"
    );
    assert!(
        home.path().join("cache").join("dogs.json").is_file(),
        "the cache lives at $SHEP_HOME/cache/dogs.json"
    );
}

#[test]
fn available_dogs_refresh_with_the_server_gone_lists_the_cache_and_says_so_once() {
    let home = TempDir::new().unwrap();
    let url = serve_dog_index(&two_entry_index_json());
    assert_success(&available_dogs(home.path(), &url, &[]));

    let output = available_dogs(home.path(), &url, &["--refresh"]);

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("shep-watchdog"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("cached"))
        .collect();
    assert_eq!(lines.len(), 1, "stderr: {stderr}");
    assert!(
        lines[0].contains(" ago"),
        "the line names the copy's age: {stderr}"
    );
}

#[test]
fn available_dogs_with_no_cache_and_no_server_refuses() {
    let home = TempDir::new().unwrap();
    // Bound then dropped, so nothing listens on the port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/dogs.json");

    let output = available_dogs(home.path(), &url, &[]);

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("reading the dog index from {url}")),
        "stderr: {stderr}"
    );
}

#[test]
fn available_dogs_says_a_cargo_row_without_a_version_is_not_installable() {
    let home = TempDir::new().unwrap();
    let index = serde_json::json!({
        "version": 1,
        "dogs": [
            {
                "name": "Bella", "package": "shep-log-rotate", "adopt_as": "log-rotate",
                "description": "Rotates logs.", "repo": "https://github.com/shep-pm/shep-log-rotate",
                "license": "MIT", "category": "logs", "source": { "kind": "cargo" }
            },
            {
                "name": "Moon", "package": "shep-deploy", "adopt_as": "deploy",
                "description": "Deploys.", "repo": "https://github.com/shep-pm/shep-deploy",
                "license": "MIT", "category": "deploy",
                "source": { "kind": "cargo", "version": "0.6.6" }
            }
        ]
    })
    .to_string();
    let url = serve_dog_index(&index);

    let output = available_dogs(home.path(), &url, &[]);

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bella = stdout
        .lines()
        .find(|l| l.contains("Bella"))
        .expect("Bella listed");
    assert!(bella.contains("not installable"), "{stdout}");
    let moon = stdout
        .lines()
        .find(|l| l.contains("Moon"))
        .expect("Moon listed");
    assert!(!moon.contains("not installable"), "{stdout}");
}

#[test]
fn dogs_refresh_without_available_is_refused() {
    let home = TempDir::new().unwrap();

    let output = shep(home.path())
        .args(["dogs", "--refresh"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "{output:?}");
}
