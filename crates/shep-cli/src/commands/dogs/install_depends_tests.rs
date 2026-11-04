//! `shep install` of a dog whose manifest names other dogs: the ones missing
//! install first, each through its own grant prompt, and a dependency the
//! index cannot supply refuses before anything is adopted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::adopt::lookup_on_path;
use super::install::{Lookup, Published, Reach, install_dog};
use super::install_depends::Catalog;
use super::install_tests::{Fixture, args, config, row, typing};
use crate::cli::Format;
use crate::dog_index::{AvailableDog, DogSourceKind};
use crate::exit::ExitCode;
use crate::fetch::RedirectPolicy;
use crate::output::Streams;

const OTEL: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock"}],"depends":{"dogs":[{"name":"log-rotate","version":">=0.1, <0.3"}]}}"#;

fn log_rotate(depends: &str) -> String {
    format!(
        r#"{{"manifest":1,"name":"log-rotate","version":"0.2.0","protocol":12,"permissions":[{{"name":"flock.read","required":true,"reason":"reads the flock"}}]{depends}}}"#
    )
}

/// The index row for `adopt_as`, built from `package` at `version`.
fn indexed(adopt_as: &str, package: &str, version: &str) -> AvailableDog {
    let mut dog = row(DogSourceKind::Cargo {
        version: Some(version.to_owned()),
        sha256: BTreeMap::new(),
    });
    dog.adopt_as = adopt_as.to_owned();
    dog.package = package.to_owned();
    dog
}

fn otel_row() -> AvailableDog {
    indexed("otel", "shep-otel", "0.2.4")
}

fn log_rotate_row(version: &str) -> AvailableDog {
    indexed("log-rotate", "shep-log-rotate", version)
}

/// A world with a `cargo` that builds whichever of `dogs` it is asked for,
/// each a script answering its own manifest, and logs the package it built.
struct World {
    fixture: Fixture,
    bin: PathBuf,
}

impl World {
    fn new(dogs: &[(&str, &str)]) -> Self {
        let fixture = Fixture::new();
        let base = fixture.dir.path();
        let (bin, built) = (base.join("stub-bin"), base.join("built-dogs"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&built).unwrap();
        for (package, manifest) in dogs {
            std::fs::write(
                built.join(package),
                format!("#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n"),
            )
            .unwrap();
        }
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  [ \"$prev\" = --root ] && root=\"$a\"\n  prev=\"$a\"\n  last=\"$a\"\ndone\necho \"$last\" >> {log}\nmkdir -p \"$root/bin\"\ncp {built}/\"$last\" \"$root/bin/$last\"\nchmod 755 \"$root/bin/$last\"\n",
            log = base.join("built.log").display(),
            built = built.display(),
        );
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, script).unwrap();
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        Self { fixture, bin }
    }

    fn reach(&self) -> Reach {
        Reach {
            cargo: lookup_on_path(Path::new("cargo"), Some(self.bin.as_os_str())),
            lookup: Lookup::Canned(Published::Unreported),
            assets: RedirectPolicy::loopback("127.0.0.1", &["localhost"]),
            lock_wait: Duration::from_millis(100),
            progress: None,
        }
    }

    /// The packages cargo built so far, in order.
    fn built(&self) -> Vec<String> {
        std::fs::read_to_string(self.fixture.dir.path().join("built.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Installs `dog` from `index`, answering the prompts with `typed`.
    async fn install(
        &self,
        dog: &AvailableDog,
        index: &[AvailableDog],
        yes: bool,
        typed: Option<&'static str>,
    ) -> (ExitCode, String) {
        let (code, _, err) = self.install_as(Format::Table, dog, index, yes, typed).await;
        (code, err)
    }

    /// As [`Self::install`], in `format`, also returning stdout.
    async fn install_as(
        &self,
        format: Format,
        dog: &AvailableDog,
        index: &[AvailableDog],
        yes: bool,
        typed: Option<&'static str>,
    ) -> (ExitCode, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = install_dog(
            &mut Streams::bare(&mut out, &mut err, format),
            &self.fixture.paths,
            &args(yes, false),
            &self.reach(),
            &mut typing(typed),
            dog,
            &Catalog::new("https://shep-pm.com/dogs.json", index),
        )
        .await;
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn adopted(&self) -> Vec<String> {
        config(&self.fixture).adopted_dog_names()
    }
}

fn plain_log_rotate() -> String {
    log_rotate("")
}

#[tokio::test]
async fn a_missing_indexed_dependency_installs_first_with_its_own_prompt() {
    let rotate = plain_log_rotate();
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &rotate)]);
    let index = [otel_row(), log_rotate_row("0.2.0")];

    let (code, err) = world
        .install(&index[0], &index, false, Some("y\ny\n"))
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(world.adopted(), ["log-rotate", "otel"]);
    let first = err.find("adopt log-rotate and grant it these?");
    let second = err.find("adopt otel and grant it these?");
    assert!(
        first.is_some() && second.is_some() && first < second,
        "{err}"
    );
    let enabled = config(&world.fixture).enabled_dog_names();
    assert!(enabled.contains(&"log-rotate".to_owned()), "{enabled:?}");
    assert!(enabled.contains(&"otel".to_owned()), "{enabled:?}");
}

#[tokio::test]
async fn a_dependency_the_index_lacks_refuses_naming_it_before_anything_is_adopted() {
    let world = World::new(&[("shep-otel", OTEL)]);
    let index = [otel_row()];

    let (code, err) = world.install(&index[0], &index, true, None).await;

    assert_eq!(code, ExitCode::NotFound, "{err}");
    assert!(err.contains("log-rotate"), "{err}");
    assert!(world.adopted().is_empty());
    assert_eq!(world.built(), ["shep-otel"]);
}

#[tokio::test]
async fn an_index_version_outside_the_declared_range_refuses_naming_the_dependency() {
    let rotate = plain_log_rotate();
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &rotate)]);
    let index = [otel_row(), log_rotate_row("0.3.0")];

    let (code, err) = world.install(&index[0], &index, true, None).await;

    assert_eq!(code, ExitCode::Failure, "{err}");
    assert!(err.contains("log-rotate"), "{err}");
    assert!(
        err.contains("0.3.0") && err.contains(">=0.1, <0.3"),
        "{err}"
    );
    assert!(world.adopted().is_empty());
    assert_eq!(world.built(), ["shep-otel"]);
}

#[tokio::test]
async fn a_cycle_refuses_naming_the_dependency_before_anything_is_adopted() {
    let back = log_rotate(r#","depends":{"dogs":[{"name":"otel","version":">=0.2"}]}"#);
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &back)]);
    let index = [otel_row(), log_rotate_row("0.2.0")];

    let (code, err) = world.install(&index[0], &index, true, None).await;

    assert_eq!(code, ExitCode::Failure, "{err}");
    assert!(err.contains("cycle") && err.contains("otel"), "{err}");
    assert!(err.contains("log-rotate"), "{err}");
    assert!(world.adopted().is_empty());
}

#[tokio::test]
async fn a_no_at_the_prompt_of_a_dependency_stops_the_install_and_leaves_it_disabled() {
    let rotate = plain_log_rotate();
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &rotate)]);
    let index = [otel_row(), log_rotate_row("0.2.0")];

    let (code, err) = world.install(&index[0], &index, false, Some("n\n")).await;

    assert_eq!(code, ExitCode::Failure, "{err}");
    assert!(err.contains("log-rotate"), "{err}");
    assert_eq!(world.adopted(), ["log-rotate"]);
    assert!(config(&world.fixture).enabled_dog_names().is_empty());
    assert!(
        !world
            .fixture
            .paths
            .dogs_dir
            .join("otel")
            .join("receipt.json")
            .exists()
    );
    assert!(
        world
            .fixture
            .paths
            .dogs_dir
            .join("log-rotate")
            .join("receipt.json")
            .exists()
    );
}

#[tokio::test]
async fn an_adopted_dependency_is_not_installed_again() {
    let rotate = plain_log_rotate();
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &rotate)]);
    let index = [otel_row(), log_rotate_row("0.2.0")];
    let (code, err) = world.install(&index[1], &index, true, None).await;
    assert_eq!(code, ExitCode::Success, "{err}");

    let (code, err) = world.install(&index[0], &index, true, None).await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(world.built(), ["shep-log-rotate", "shep-otel"]);
    assert_eq!(world.adopted(), ["log-rotate", "otel"]);
}

/// otel needs `log-rotate` and `second`, and `log-rotate` needs `second` too.
const DIAMOND_OTEL: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[],"depends":{"dogs":[{"name":"log-rotate","version":">=0.1"},{"name":"second","version":">=0.1"}]}}"#;

fn second(depends: &str) -> String {
    log_rotate(depends).replace("log-rotate", "second")
}

#[tokio::test]
async fn a_dependency_two_dogs_share_is_built_and_asked_about_once() {
    let rotate = log_rotate(r#","depends":{"dogs":[{"name":"second","version":">=0.1"}]}"#);
    let world = World::new(&[
        ("shep-otel", DIAMOND_OTEL),
        ("shep-log-rotate", &rotate),
        ("shep-second", &second("")),
    ]);
    let index = [
        otel_row(),
        log_rotate_row("0.2.0"),
        indexed("second", "shep-second", "0.2.0"),
    ];

    let (code, err) = world
        .install(&index[0], &index, false, Some("y\ny\ny\n"))
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(err.matches("adopt second and grant it these?").count(), 1);
    let mut built = world.built();
    built.sort();
    assert_eq!(built, ["shep-log-rotate", "shep-otel", "shep-second"]);
    assert_eq!(world.adopted(), ["second", "log-rotate", "otel"]);
}

#[tokio::test]
async fn a_dependency_of_a_dependency_the_index_lacks_refuses_before_any_adopt() {
    let rotate = plain_log_rotate();
    let world = World::new(&[
        ("shep-otel", DIAMOND_OTEL),
        ("shep-log-rotate", &rotate),
        (
            "shep-second",
            &second(r#","depends":{"dogs":[{"name":"gone","version":">=0.1"}]}"#),
        ),
    ]);
    let index = [
        otel_row(),
        log_rotate_row("0.2.0"),
        indexed("second", "shep-second", "0.2.0"),
    ];

    let (code, err) = world.install(&index[0], &index, true, None).await;

    assert_eq!(code, ExitCode::NotFound, "{err}");
    assert!(
        err.contains("gone") && err.contains("nothing was adopted"),
        "{err}"
    );
    assert!(world.adopted().is_empty());
    assert!(!err.contains("grant it these?"), "{err}");
}

#[tokio::test]
async fn json_answers_one_envelope_for_the_dog_not_one_per_dependency() {
    let rotate = plain_log_rotate();
    let world = World::new(&[("shep-otel", OTEL), ("shep-log-rotate", &rotate)]);
    let index = [otel_row(), log_rotate_row("0.2.0")];

    let (code, out, err) = world
        .install_as(Format::Json, &index[0], &index, true, None)
        .await;

    assert_eq!(code, ExitCode::Success, "{err}");
    assert_eq!(world.adopted(), ["log-rotate", "otel"]);
    let envelope: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!("stdout is not one JSON document ({e}): {out}");
    });
    assert!(envelope.to_string().contains("otel"), "{envelope}");
    assert!(!envelope.to_string().contains("log-rotate"), "{envelope}");
}
