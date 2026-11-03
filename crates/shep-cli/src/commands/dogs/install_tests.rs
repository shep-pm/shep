//! `shep install` end to end with a stub `cargo` and a stub crates.io: the
//! command it runs, the receipt it leaves, and what it refuses before it
//! builds anything.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Duration;

use shep_core::file_lock::FileLock;
use shep_core::paths::ShepPaths;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::consent::Input;
use super::install::{Lookup, Published, Reach, install_dog, published_from};
use super::test_support::streams;
use crate::cli::InstallArgs;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;

const MANIFEST: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"}]}"#;

const REPO: &str = "https://github.com/shep-pm/shep-otel";

/// The publisher the row's own repo names.
fn from_the_row() -> Published {
    Published::From("shep-pm/shep-otel".to_owned())
}

struct Fixture {
    dir: tempfile::TempDir,
    paths: ShepPaths,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        Self { dir, paths }
    }

    /// Where the stub cargo records the arguments it was run with.
    fn argv(&self) -> PathBuf {
        self.dir.path().join("argv")
    }

    /// A directory holding a `cargo` that records its arguments and builds
    /// the dog into the `--root` it is given, named for the last argument.
    fn stub_cargo(&self) -> PathBuf {
        let bin = self.dir.path().join("stub-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dog = self.dir.path().join("built-dog");
        std::fs::write(
            &dog,
            format!("#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{MANIFEST}\nEOF\nfi\nexit 0\n"),
        )
        .unwrap();
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nprev=\nfor a in \"$@\"; do\n  [ \"$prev\" = --root ] && root=\"$a\"\n  prev=\"$a\"\n  last=\"$a\"\ndone\nmkdir -p \"$root/bin\"\ncp {dog} \"$root/bin/$last\"\nchmod 755 \"$root/bin/$last\"\n",
            argv = self.argv().display(),
            dog = dog.display(),
        );
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, script).unwrap();
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        bin
    }

    fn reach(&self, published: Published) -> Reach {
        let bin = self.stub_cargo();
        Reach {
            // The real PATH lookup, over a PATH holding only the stub.
            cargo: super::adopt::lookup_on_path(Path::new("cargo"), Some(bin.as_os_str())),
            lookup: Lookup::Canned(published),
            lock_wait: Duration::from_millis(100),
        }
    }
}

fn row(source: DogSourceKind) -> AvailableDog {
    AvailableDog {
        name: "Otel".to_owned(),
        package: "shep-otel".to_owned(),
        adopt_as: "otel".to_owned(),
        description: "exports traces".to_owned(),
        repo: REPO.to_owned(),
        license: "MIT".to_owned(),
        category: "metrics".to_owned(),
        source,
    }
}

fn cargo_row() -> AvailableDog {
    row(DogSourceKind::Cargo {
        version: Some("0.2.4".to_owned()),
        sha256: BTreeMap::new(),
    })
}

/// Serves `body` as a 200 to every request, on an ephemeral loopback port,
/// and sends each request it reads down the returned channel.
async fn crates_io(body: String) -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (seen, asked) = tokio::sync::mpsc::unbounded_channel();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _peer) = listener.accept().await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let mut buf = [0u8; 2048];
            let read = stream.read(&mut buf).await.unwrap_or(0);
            let _ = seen.send(String::from_utf8_lossy(&buf[..read]).into_owned());
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (format!("http://127.0.0.1:{}", addr.port()), asked)
}

fn typing(typed: Option<&'static str>) -> Input {
    Input {
        is_terminal: typed.is_some(),
        reader: Box::new(Cursor::new(typed.unwrap_or("").as_bytes())),
    }
}

fn args(yes: bool, no_enable: bool) -> InstallArgs {
    InstallArgs {
        dog: Some("otel".to_owned()),
        yes,
        no_enable,
        refresh: false,
    }
}

async fn run(
    fixture: &Fixture,
    dog: &AvailableDog,
    args: &InstallArgs,
    reach: &Reach,
    mut input: Input,
) -> (ExitCode, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = install_dog(
        &mut streams(&mut out, &mut err),
        &fixture.paths,
        args,
        reach,
        &mut input,
        dog,
        "https://shep-pm.com/dogs.json",
    )
    .await;
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn config(fixture: &Fixture) -> ShepToml {
    ShepToml::read_only(&fixture.paths.daemon_config).unwrap()
}

fn enabled(fixture: &Fixture) -> bool {
    config(fixture)
        .enabled_dog_names()
        .iter()
        .any(|dog| dog == "otel")
}

#[tokio::test]
async fn it_runs_the_exact_cargo_command_records_a_receipt_and_adopts_under_adopt_as() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let root = fixture.paths.dogs_dir.join("otel").join("0.2.4");
    let argv = std::fs::read_to_string(fixture.argv()).unwrap();
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        [
            "install",
            "--locked",
            "--version",
            "0.2.4",
            "--root",
            root.to_str().unwrap(),
            "shep-otel"
        ]
    );

    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.paths.dogs_dir.join("otel").join("receipt.json")).unwrap(),
    )
    .unwrap();
    let binary = root.join("bin").join("shep-otel");
    assert_eq!(receipt["index"], "https://shep-pm.com/dogs.json");
    assert_eq!(receipt["row"]["adopt_as"], "otel");
    assert_eq!(receipt["row"]["repo"], REPO);
    assert_eq!(receipt["row"]["source"]["kind"], "cargo");
    assert_eq!(receipt["package"], "shep-otel");
    assert_eq!(receipt["version"], "0.2.4");
    assert_eq!(receipt["command"][0], "cargo");
    assert_eq!(receipt["command"][7], "shep-otel");
    assert_eq!(receipt["binary"], binary.to_str().unwrap());
    assert_eq!(
        receipt["sha256"],
        crate::commands::dogs::vet::sha256_hex(&binary).unwrap()
    );
    assert_eq!(receipt["manifest"]["name"], "otel");
    assert!(receipt["installed_at"].as_str().unwrap().ends_with('Z'));

    let adopted = config(&fixture).adopted_dog_path("otel").unwrap();
    assert_eq!(adopted, binary.canonicalize().unwrap());
    assert!(enabled(&fixture));
}

#[tokio::test]
async fn a_publisher_other_than_the_rows_repo_refuses_before_cargo_runs() {
    let fixture = Fixture::new();
    let reach = fixture.reach(Published::From("someone-else/shep-otel".to_owned()));

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("someone-else/shep-otel"), "{err}");
    assert!(err.contains(REPO), "{err}");
    assert!(!fixture.argv().exists(), "cargo must not have run");
    assert!(!fixture.paths.dogs_dir.join("otel").exists());
    assert!(config(&fixture).adopted_dog_path("otel").is_none());
}

#[tokio::test]
async fn a_token_publish_adds_one_line_to_the_prompt_and_still_installs() {
    let fixture = Fixture::new();
    let reach = fixture.reach(Published::ByToken);

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let lines: Vec<_> = err
        .lines()
        .filter(|line| line.contains("API token"))
        .collect();
    assert_eq!(lines.len(), 1, "{err}");
    assert!(fixture.argv().exists());
}

#[tokio::test]
async fn a_response_without_trustpub_data_skips_the_check_and_adds_no_line() {
    let fixture = Fixture::new();
    let reach = fixture.reach(Published::Unreported);

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(!err.contains("API token"), "{err}");
    assert!(fixture.argv().exists());
}

#[tokio::test]
async fn no_cargo_on_path_refuses_naming_cargo_and_rustup() {
    let fixture = Fixture::new();
    let empty = fixture.dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let reach = Reach {
        cargo: super::adopt::lookup_on_path(Path::new("cargo"), Some(empty.as_os_str())),
        lookup: Lookup::Canned(from_the_row()),
        lock_wait: Duration::from_millis(100),
    };

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("cargo"), "{err}");
    assert!(err.contains("https://rustup.rs"), "{err}");
    assert!(!fixture.paths.dogs_dir.join("otel").exists());
}

#[tokio::test]
async fn a_no_at_the_grant_prompt_leaves_the_dog_installed_adopted_and_disabled() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(false, false),
        &reach,
        typing(Some("n\n")),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(err.contains("flock.read"), "{err}");
    assert!(fixture.paths.dogs_dir.join("otel/receipt.json").exists());
    assert!(config(&fixture).adopted_dog_path("otel").is_some());
    assert!(!enabled(&fixture));
}

#[tokio::test]
async fn no_enable_adopts_and_grants_but_never_enables() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, true),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    let cfg = config(&fixture);
    assert!(cfg.adopted_dog_path("otel").is_some());
    assert!(cfg.granted("otel").unwrap().is_some(), "the grant stands");
    assert!(!enabled(&fixture));
}

#[tokio::test]
async fn a_second_install_while_the_lock_is_held_refuses_naming_the_lock() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    std::fs::create_dir_all(&fixture.paths.dogs_dir).unwrap();
    let _held = FileLock::acquire(&fixture.paths.dogs_dir.join("otel")).unwrap();

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(true, false),
        &reach,
        typing(None),
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(err.contains("otel.lock"), "{err}");
    assert!(!fixture.argv().exists(), "cargo must not have run");
}

#[tokio::test]
async fn a_row_that_is_not_a_versioned_cargo_row_prints_its_instructions_and_installs_nothing() {
    let rows = [
        (
            row(DogSourceKind::GoInstall {
                module: "example.com/otel".to_owned(),
            }),
            "go install example.com/otel@latest",
        ),
        (
            row(DogSourceKind::Manual {
                instructions: "download it from the releases page".to_owned(),
            }),
            "download it from the releases page",
        ),
        (
            row(DogSourceKind::Cargo {
                version: None,
                sha256: BTreeMap::new(),
            }),
            "not installable",
        ),
        (
            row(DogSourceKind::CargoGit {
                url: "https://github.com/shep-pm/shep-otel".to_owned(),
            }),
            "cargo install --git",
        ),
    ];
    for (dog, instruction) in rows {
        let fixture = Fixture::new();
        let reach = fixture.reach(from_the_row());

        let (code, out, _err) = run(&fixture, &dog, &args(true, false), &reach, typing(None)).await;

        assert_eq!(code, ExitCode::Failure, "{instruction}");
        assert!(out.contains(instruction), "{out}");
        assert!(!fixture.argv().exists(), "cargo must not have run");
        assert!(!fixture.paths.dogs_dir.exists());
        assert!(config(&fixture).adopted_dog_path("otel").is_none());
    }
}

#[tokio::test]
async fn a_row_that_cannot_install_answers_json_with_an_error_envelope_and_no_table() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let dog = row(DogSourceKind::GoInstall {
        module: "example.com/otel".to_owned(),
    });
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut streams = crate::output::Streams::bare(&mut out, &mut err, crate::cli::Format::Json);

    let code = install_dog(
        &mut streams,
        &fixture.paths,
        &args(true, false),
        &reach,
        &mut typing(None),
        &dog,
        "https://shep-pm.com/dogs.json",
    )
    .await;

    assert_eq!(code, ExitCode::Failure);
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    let envelope: serde_json::Value = serde_json::from_slice(&err).unwrap();
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("go install example.com/otel@latest"),
        "{envelope}"
    );
    assert!(!fixture.paths.dogs_dir.exists());
}

#[tokio::test]
async fn a_row_whose_name_would_climb_out_of_the_dogs_directory_is_refused() {
    let fixture = Fixture::new();
    let reach = fixture.reach(from_the_row());
    let mut dog = cargo_row();
    dog.adopt_as = "../escape".to_owned();

    let (code, _out, _err) = run(&fixture, &dog, &args(true, false), &reach, typing(None)).await;

    assert_eq!(code, ExitCode::InvalidConfig);
    assert!(!fixture.argv().exists(), "cargo must not have run");
}

/// The wire half of the lookup, against a stub crates.io: the path asked
/// for, and each of the three answers read.
#[tokio::test]
async fn the_lookup_asks_crates_io_for_the_version_and_reads_all_three_answers() {
    let from = r#"{"version":{"trustpub_data":{"provider":"github","repository":"o/r"}}}"#;
    let token = r#"{"version":{"trustpub_data":null}}"#;
    let silent = r#"{"version":{"num":"0.2.4"}}"#;
    for (body, expected) in [
        (from, Published::From("o/r".to_owned())),
        (token, Published::ByToken),
        (silent, Published::Unreported),
    ] {
        let (base, mut asked) = crates_io(body.to_owned()).await;

        let published = published_from(&base, "shep-otel", "0.2.4").await.unwrap();

        assert_eq!(published, expected);
        let request = asked.recv().await.unwrap();
        assert!(
            request.starts_with("GET /api/v1/crates/shep-otel/0.2.4 HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains("User-Agent: shep/"), "{request}");
    }
}
