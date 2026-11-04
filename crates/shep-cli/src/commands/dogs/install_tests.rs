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
use super::install_depends::Catalog;
use super::test_support::streams;
use crate::cli::InstallArgs;
use crate::commands::shep_toml::ShepToml;
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;
use crate::fetch::RedirectPolicy;

pub(super) const MANIFEST: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock to report on it"}]}"#;

/// The commit the stub cargo says it built.
pub(super) const COMMIT: &str = "3f9c2a1b7d4e5f60718293a4b5c6d7e8f9012345";

/// A newer commit, for a test that moves a ref.
pub(super) const NEWER_COMMIT: &str = "9a8b7c6d5e4f30211203948576a6b5c4d3e2f101";

pub(super) const REPO: &str = "https://github.com/shep-pm/shep-otel";

/// The publisher the row's own repo names.
pub(super) fn from_the_row() -> Published {
    Published::From("shep-pm/shep-otel".to_owned())
}

pub(super) struct Fixture {
    pub(super) dir: tempfile::TempDir,
    pub(super) paths: ShepPaths,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        Self { dir, paths }
    }

    /// Where the stub cargo records the arguments it was run with.
    pub(super) fn argv(&self) -> PathBuf {
        self.dir.path().join("argv")
    }

    /// Where the stub cargo appends one line per run.
    pub(super) fn attempts(&self) -> PathBuf {
        self.dir.path().join("attempts")
    }

    /// Makes the stub cargo find this tag.
    pub(super) fn has_tag(&self, tag: &str) {
        std::fs::write(self.dir.path().join("tags"), format!("{tag}\n")).unwrap();
    }

    /// Makes the stub cargo report a `--git` build at `commit`.
    pub(super) fn builds_commit(&self, commit: &str) {
        std::fs::write(self.dir.path().join("commit"), commit).unwrap();
    }

    /// Makes the stub cargo build a dog whose `--shep-manifest` prints `manifest`.
    pub(super) fn builds_manifest(&self, manifest: &str) {
        let dog = self.dir.path().join("built-dog");
        std::fs::write(
            &dog,
            format!("#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n"),
        )
        .unwrap();
    }

    /// A directory holding a `cargo` that records its arguments and builds
    /// the dog into the `--root` it is given, named for the last argument.
    fn stub_cargo(&self) -> PathBuf {
        let bin = self.dir.path().join("stub-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dog = self.dir.path().join("built-dog");
        // A test that chose what the build declares keeps it across `reach`es.
        if !dog.exists() {
            self.builds_manifest(MANIFEST);
        }
        // Every run is appended to `attempts`; `argv` holds the last. A `--tag`
        // succeeds only for a name listed in `tags`. A `--git` install writes
        // the `.crates2.json` cargo would.
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > {argv}
printf '%s\n' "$*" >> {attempts}
commit=$(cat {commit_file} 2>/dev/null || echo {commit})
prev=
for a in "$@"; do
  [ "$prev" = --root ] && root="$a"
  [ "$prev" = --tag ] && tag="$a"
  [ "$prev" = --branch ] && kind=branch && wanted="$a"
  [ "$prev" = --rev ] && kind=rev && wanted="$a"
  [ "$a" = --git ] && git=1
  prev="$a"
  last="$a"
done
if [ -n "$tag" ]; then
  grep -qx "$tag" {tags} 2>/dev/null || exit 101
  kind=tag; wanted="$tag"
fi
mkdir -p "$root/bin"
cp {dog} "$root/bin/$last"
chmod 755 "$root/bin/$last"
if [ -n "$git" ]; then
  printf '{{"installs":{{"%s 0.2.4 (git+{repo}?%s=%s#%s)":{{"bins":["%s"]}}}}}}' "$last" "$kind" "$wanted" "$commit" "$last" > "$root/.crates2.json"
fi
"#,
            argv = self.argv().display(),
            attempts = self.attempts().display(),
            tags = self.dir.path().join("tags").display(),
            dog = dog.display(),
            repo = REPO,
            commit = COMMIT,
            commit_file = self.dir.path().join("commit").display(),
        );
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, script).unwrap();
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        bin
    }

    pub(super) fn reach(&self, published: Published) -> Reach {
        let bin = self.stub_cargo();
        Reach {
            // The real PATH lookup, over a PATH holding only the stub.
            cargo: super::adopt::lookup_on_path(Path::new("cargo"), Some(bin.as_os_str())),
            lookup: Lookup::Canned(published),
            assets: RedirectPolicy::loopback("127.0.0.1", &["localhost"]),
            lock_wait: Duration::from_millis(100),
        }
    }
}

pub(super) fn row(source: DogSourceKind) -> AvailableDog {
    AvailableDog {
        name: "Otel".to_owned(),
        package: "shep-otel".to_owned(),
        adopt_as: "otel".to_owned(),
        description: "exports traces".to_owned(),
        repo: REPO.to_owned(),
        license: "MIT".to_owned(),
        category: "metrics".to_owned(),
        permissions: None,
        needs: Vec::new(),
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

pub(super) fn typing(typed: Option<&'static str>) -> Input {
    Input::reading(typed.is_some(), Cursor::new(typed.unwrap_or("").as_bytes()))
}

pub(super) fn args(yes: bool, no_enable: bool) -> InstallArgs {
    InstallArgs {
        dog: Some("otel".to_owned()),
        yes,
        no_enable,
        refresh: false,
        cargo: false,
        git_ref: None,
    }
}

pub(super) async fn run(
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
        &Catalog::new("https://shep-pm.com/dogs.json", &[]),
    )
    .await;
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

pub(super) fn config(fixture: &Fixture) -> ShepToml {
    ShepToml::read_only(&fixture.paths.daemon_config).unwrap()
}

pub(super) fn enabled(fixture: &Fixture) -> bool {
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
async fn a_token_publish_puts_one_line_directly_above_the_grant_question() {
    let fixture = Fixture::new();
    let reach = fixture.reach(Published::ByToken);

    let (code, _out, err) = run(
        &fixture,
        &cargo_row(),
        &args(false, false),
        &reach,
        typing(Some("n\n")),
    )
    .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert!(fixture.argv().exists());
    let lines: Vec<_> = err.lines().collect();
    let question = lines
        .iter()
        .position(|line| line.contains("[y/N]"))
        .unwrap_or_else(|| panic!("no question was asked:\n{err}"));
    assert!(
        lines[question - 1].contains("API token"),
        "the line must sit right above the question:\n{err}"
    );
    assert_eq!(err.matches("API token").count(), 1, "{err}");
}

#[tokio::test]
async fn a_token_publish_is_still_said_once_when_yes_asks_nothing() {
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
    assert_eq!(err.matches("API token").count(), 1, "{err}");
    assert!(!err.contains("[y/N]"), "{err}");
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
        assets: RedirectPolicy::GITHUB_RELEASES,
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
        &Catalog::new("https://shep-pm.com/dogs.json", &[]),
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
