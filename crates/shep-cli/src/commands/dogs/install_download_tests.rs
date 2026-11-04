//! `shep install` on the download path: the hash it checks, the redirect it
//! follows, what it leaves on disk, and when it falls back to cargo.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::install::{Lookup, Published, Reach};
use super::install_tests::{
    Fixture, MANIFEST, REPO, args, config, enabled, from_the_row, row, run, typing,
};
use super::session::{Progress, Step};
use crate::cli::InstallArgs;
use crate::dog_index::{AvailableDog, BUILD_TARGET, DogSourceKind};
use crate::exit::ExitCode;
use crate::fetch::RedirectPolicy;

/// A dog that prints `MANIFEST` and exits, as a shell script.
fn dog_binary() -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{MANIFEST}\nEOF\nfi\nexit 0\n"
    )
    .into_bytes()
}

/// Lowercase hex sha256, from ring directly so no shep helper vouches for itself.
fn sha256_of(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Answers each request with `answer(path, port)`, on an ephemeral loopback
/// port, and sends every request path down the returned channel.
async fn serve(
    answer: impl Fn(&str, u16) -> Vec<u8> + Send + 'static,
) -> (u16, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (seen, asked) = tokio::sync::mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut stream, _peer) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let read = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..read]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
            let _ = stream.write_all(&answer(&path, port)).await;
            let _ = stream.shutdown().await;
            let _ = seen.send(path);
        }
    });
    (port, asked)
}

fn ok(body: &[u8]) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

fn redirect(location: &str) -> Vec<u8> {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n").into_bytes()
}

/// A row whose repo is `repo` and which hashes `hash` for this build's target.
fn prebuilt_row(repo: &str, hash: &str) -> AvailableDog {
    let mut dog = row(DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: BTreeMap::from([(BUILD_TARGET.to_owned(), hash.to_owned())]),
    });
    repo.clone_into(&mut dog.repo);
    dog
}

fn asset_url(port: u16) -> String {
    format!(
        "http://127.0.0.1:{port}/releases/download/v0.2.4/shep-otel-0.2.4-{BUILD_TARGET}{}",
        if BUILD_TARGET.contains("windows") {
            ".exe"
        } else {
            ""
        }
    )
}

fn installed_binary(fixture: &Fixture) -> std::path::PathBuf {
    fixture.paths.dogs_dir.join("otel/0.2.4/bin/shep-otel")
}

fn receipt(fixture: &Fixture) -> serde_json::Value {
    let bytes = std::fs::read(fixture.paths.dogs_dir.join("otel/receipt.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn with_cargo_flag() -> InstallArgs {
    InstallArgs {
        cargo: true,
        ..args(true, false)
    }
}

#[tokio::test]
async fn a_matching_hash_installs_the_binary_and_the_receipt_carries_the_url() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let body = binary.clone();
    let (port, _asked) = serve(move |_path, _port| ok(&body)).await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &sha256_of(&binary));

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &args(true, false),
        &fixture.reach(from_the_row()),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!fixture.argv().exists(), "cargo must not have run");
    assert_eq!(std::fs::read(installed_binary(&fixture)).unwrap(), binary);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(installed_binary(&fixture))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "{mode:o}");
    }
    let receipt = receipt(&fixture);
    assert_eq!(receipt["url"], asset_url(port));
    assert!(receipt.get("command").is_none(), "{receipt}");
    assert_eq!(receipt["sha256"], sha256_of(&binary));
    assert_eq!(receipt["version"], "0.2.4");
    assert_eq!(receipt["manifest"]["name"], "otel");
    assert!(config(&fixture).adopted_dog_path("otel").is_some());
    assert!(enabled(&fixture));
}

#[tokio::test]
async fn a_mismatched_hash_refuses_naming_both_and_leaves_nothing_behind() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let wrong = sha256_of(b"some other binary");
    let body = binary.clone();
    let (port, _asked) = serve(move |_path, _port| ok(&body)).await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &wrong);

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &args(true, false),
        &fixture.reach(from_the_row()),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains(&wrong), "{err}");
    assert!(err.contains(&sha256_of(&binary)), "{err}");
    assert!(!fixture.paths.dogs_dir.join("otel").exists());
    let strays: Vec<_> = std::fs::read_dir(&fixture.paths.dogs_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".lock"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
    assert!(config(&fixture).adopted_dog_path("otel").is_none());
}

#[tokio::test]
async fn one_redirect_to_the_asset_host_is_followed_to_the_binary() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let body = binary.clone();
    let (port, mut asked) = serve(move |path, port| {
        if path == "/asset" {
            ok(&body)
        } else {
            redirect(&format!("http://localhost:{port}/asset"))
        }
    })
    .await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &sha256_of(&binary));

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &args(true, false),
        &fixture.reach(from_the_row()),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(std::fs::read(installed_binary(&fixture)).unwrap(), binary);
    let first = asked.recv().await.unwrap();
    assert!(first.starts_with("/releases/download/v0.2.4/"), "{first}");
    assert_eq!(asked.recv().await.unwrap(), "/asset");
    assert_eq!(receipt(&fixture)["url"], asset_url(port));
}

#[tokio::test]
async fn a_redirect_off_the_asset_host_refuses_and_leaves_nothing_behind() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let (port, _asked) = serve(move |_path, _port| redirect("http://elsewhere.test/asset")).await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &sha256_of(&binary));

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &args(true, false),
        &fixture.reach(from_the_row()),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("http://elsewhere.test/asset"), "{err}");
    assert!(!fixture.paths.dogs_dir.join("otel").exists());
    assert!(
        !fixture.argv().exists(),
        "a refused download must not fall back to cargo"
    );
}

#[tokio::test]
async fn a_row_with_no_hash_for_this_target_builds_with_cargo() {
    let fixture = Fixture::new();
    let mut dog = prebuilt_row(REPO, &sha256_of(b"x"));
    dog.source = DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: BTreeMap::from([("some-other-target".to_owned(), sha256_of(b"x"))]),
    };

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &args(true, false),
        &fixture.reach(from_the_row()),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(fixture.argv().exists(), "cargo should have run");
    let receipt = receipt(&fixture);
    assert_eq!(receipt["command"][0], "cargo");
    assert!(receipt.get("url").is_none(), "{receipt}");
}

#[tokio::test]
async fn a_prebuilt_download_tells_the_sink_it_is_downloading_and_never_building() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let body = binary.clone();
    let (port, _asked) = serve(move |_path, _port| ok(&body)).await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &sha256_of(&binary));
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    let mut reach = fixture.reach(from_the_row());
    reach.progress = Some(Progress::new(move |step| sink.lock().unwrap().push(step)));

    let (code, _out, err) = run(&fixture, &dog, &args(true, false), &reach, typing(None)).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(*heard.lock().unwrap(), [Step::Downloading, Step::Adopting]);
}

#[tokio::test]
async fn no_hash_for_this_target_and_no_cargo_refuses_naming_both_and_rustup() {
    let fixture = Fixture::new();
    let empty = fixture.dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let reach = Reach {
        cargo: super::adopt::lookup_on_path(Path::new("cargo"), Some(empty.as_os_str())),
        lookup: Lookup::Canned(from_the_row()),
        assets: RedirectPolicy::GITHUB_RELEASES,
        lock_wait: Duration::from_millis(100),
        progress: None,
    };

    let (code, _out, err) = run(
        &fixture,
        &row(DogSourceKind::Cargo {
            version: Some("0.2.4".to_owned()),
            sha256: BTreeMap::new(),
        }),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert_eq!(err.trim().lines().count(), 1, "{err}");
    assert!(err.contains("no prebuilt"), "{err}");
    assert!(err.contains(BUILD_TARGET), "{err}");
    assert!(err.contains("`cargo`"), "{err}");
    assert!(err.contains("https://rustup.rs"), "{err}");
    assert!(!fixture.paths.dogs_dir.join("otel").exists());
}

#[tokio::test]
async fn the_cargo_flag_builds_even_when_a_prebuilt_exists() {
    let fixture = Fixture::new();
    let binary = dog_binary();
    let body = binary.clone();
    let (port, mut asked) = serve(move |_path, _port| ok(&body)).await;
    let dog = prebuilt_row(&format!("http://127.0.0.1:{port}"), &sha256_of(&binary));

    let (code, _out, err) = run(
        &fixture,
        &dog,
        &with_cargo_flag(),
        &fixture.reach(Published::Unreported),
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(fixture.argv().exists(), "cargo should have run");
    assert!(
        asked.try_recv().is_err(),
        "nothing should have been downloaded"
    );
    let receipt = receipt(&fixture);
    assert_eq!(receipt["command"][0], "cargo");
    assert!(receipt.get("url").is_none(), "{receipt}");
}

#[tokio::test]
async fn a_reinstall_that_adopt_refuses_leaves_the_enabled_dogs_binary_and_receipt_as_they_were() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let row = row(DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: BTreeMap::new(),
    });
    let (code, _out, err) = run(&fixture, &row, &args(true, false), &reach, typing(None)).await;
    assert_eq!(code, ExitCode::Success, "{err}");
    let (binary, receipt_file) = (
        installed_binary(&fixture),
        fixture.paths.dogs_dir.join("otel/receipt.json"),
    );
    let (kept_binary, kept_receipt) = (
        std::fs::read(&binary).unwrap(),
        std::fs::read(&receipt_file).unwrap(),
    );

    // A different build this time, one that requires a permission this shep
    // does not know, which `adopt` refuses.
    let unknown = MANIFEST.replace(
        "\"permissions\":[",
        "\"permissions\":[{\"name\":\"future.thing\",\"required\":true,\"reason\":\"x\"},",
    );
    let rebuilt = String::from_utf8(dog_binary())
        .unwrap()
        .replace(MANIFEST, &unknown);
    std::fs::write(fixture.dir.path().join("built-dog"), rebuilt).unwrap();
    let (code, _out, _err) = run(&fixture, &row, &args(true, false), &reach, typing(None)).await;

    assert_ne!(code, ExitCode::Success);
    assert_eq!(std::fs::read(&binary).unwrap(), kept_binary);
    assert_eq!(std::fs::read(&receipt_file).unwrap(), kept_receipt);
    assert!(enabled(&fixture));
    let parked: Vec<_> = std::fs::read_dir(fixture.paths.dogs_dir.join("otel"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        parked
            .iter()
            .all(|name| !name.to_string_lossy().contains("kept")),
        "{parked:?}"
    );
}
