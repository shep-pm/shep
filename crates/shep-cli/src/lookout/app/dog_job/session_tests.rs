//! Dog jobs run end to end against a stub `cargo`: the questions a session asks
//! reach the dashboard as modals, and a run that ends stops its build.

use std::path::Path;
use std::time::Duration;

use shep_core::paths::ShepPaths;

use super::tests::{grant_question, grant_tag, installing};
use super::*;
use crate::lookout::testing::{is_gone, sleeping_cargo};

/// The manifest `otel` answers with: it needs `log-rotate`.
const OTEL: &str = r#"{"manifest":1,"name":"otel","version":"0.2.4","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock"}],"depends":{"dogs":[{"name":"log-rotate","version":">=0.1, <0.3"}]}}"#;

/// The manifest `log-rotate` answers with.
const LOG_ROTATE: &str = r#"{"manifest":1,"name":"log-rotate","version":"0.2.0","protocol":12,"permissions":[{"name":"flock.read","required":true,"reason":"reads the flock"}]}"#;

/// A home with a cached index holding `otel` and `log-rotate`, and a `cargo`
/// that builds either package and answers with its own manifest.
struct World {
    _dir: tempfile::TempDir,
    paths: ShepPaths,
    cargo: std::path::PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp home");
        let paths = ShepPaths::resolve(&|_| None, dir.path());
        let (bin, built) = (dir.path().join("stub-bin"), dir.path().join("built-dogs"));
        std::fs::create_dir_all(&bin).expect("the stub bin");
        std::fs::create_dir_all(&built).expect("the built dogs");
        for (package, manifest) in [("shep-otel", OTEL), ("shep-log-rotate", LOG_ROTATE)] {
            std::fs::write(
                built.join(package),
                format!("#!/bin/sh\nif [ \"$1\" = --shep-manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nfi\nexit 0\n"),
            )
            .expect("a built dog");
        }
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  [ \"$prev\" = --root ] && root=\"$a\"\n  prev=\"$a\"\n  last=\"$a\"\ndone\nmkdir -p \"$root/bin\"\ncp {built}/\"$last\" \"$root/bin/$last\"\nchmod 755 \"$root/bin/$last\"\n",
            built = built.display(),
        );
        let cargo = bin.join("cargo");
        std::fs::write(&cargo, script).expect("the stub cargo");
        std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("an executable stub");
        let document = serde_json::json!({
            "version": 1,
            "dogs": [
                {
                    "name": "Otel", "package": "shep-otel", "adopt_as": "otel",
                    "description": "exports traces",
                    "repo": "https://github.com/shep-pm/shep-otel",
                    "license": "MIT", "category": "metrics",
                    "source": { "kind": "cargo", "version": "0.2.4" }
                },
                {
                    "name": "Log rotate", "package": "shep-log-rotate", "adopt_as": "log-rotate",
                    "description": "rotates logs",
                    "repo": "https://github.com/shep-pm/shep-log-rotate",
                    "license": "MIT", "category": "logs",
                    "source": { "kind": "cargo", "version": "0.2.0" }
                }
            ]
        });
        crate::dog_index::write_cache(
            &paths.dog_index_cache,
            &crate::dog_index::index_url(),
            document.to_string().as_bytes(),
        )
        .expect("a cached index");
        Self {
            _dir: dir,
            paths,
            cargo,
        }
    }
}

/// How long a session may take before the test calls it stuck: a stub cargo
/// and a canned crates.io, so this is a generous bound rather than a guess.
const SESSION_WAIT: Duration = Duration::from_secs(30);

/// Drives one job end to end, answering every grant question with `yes`.
///
/// Not `start_paused`: the job runs on its own thread against its own real
/// clock, and a paused test clock would fire every `timeout` here instantly.
#[tokio::test]
async fn dependencies_are_asked_in_the_clis_order_and_a_no_stops_the_next_question() {
    let world = World::new();
    let (mut app, _) = installing();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    app.note_modal_reach(Some(0));

    let mut asked: Vec<String> = Vec::new();
    let mut ended: Option<(bool, String)> = None;
    while ended.is_none() {
        let msg = tokio::time::timeout(SESSION_WAIT, run.msgs.recv())
            .await
            .expect("the job reported within the bound")
            .expect("the job's channel stays open until the run ends");
        if let Msg::DogJobDone { ok, output, .. } = &msg {
            ended = Some((*ok, output.clone()));
        }
        app.update(msg);
        // Answer every question the job just asked, with a no.
        while let Some(tag) = grant_tag(&app) {
            asked.push(grant_question(&app));
            app.note_modal_reach(Some(0));
            assert_eq!(
                app.update(Msg::Key(KeyPress::Escape)),
                Effect::ModalAnswered { tag, yes: false }
            );
            app.answer_grant(tag, false);
        }
    }

    let (ok, output) = ended.expect("the job ended");
    assert_eq!(asked.len(), 1, "only the dependency was asked: {asked:?}");
    assert!(asked[0].contains("log-rotate"), "{asked:?}");
    assert!(!ok, "{output}");
    assert!(output.contains("you said no to it"), "{output}");
    assert_eq!(app.dog_job_line(), None, "the bar is clear");
    assert!(
        !world.paths.dogs_dir.join("otel/receipt.json").exists(),
        "a stopped install writes no receipt"
    );
}

/// Enter on an adopted entry asks the swap question in a grant modal that
/// names the adopted path, and a no builds nothing and changes nothing.
#[tokio::test]
async fn a_swap_asks_a_modal_naming_the_adopted_path_and_a_no_changes_nothing() {
    use crate::commands::shep_toml::ShepToml;

    let world = World::new();
    let adopted = world._dir.path().join("adopted-otel");
    std::fs::write(&adopted, "old binary").expect("an adopted file");
    ShepToml::try_edit(&world.paths.daemon_config, |cfg| {
        cfg.adopt_dog("otel", &adopted, false)
    })
    .expect("an adopted dog");
    let before = std::fs::read(&world.paths.daemon_config).expect("the config");
    let (mut app, _) = installing();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    app.note_modal_reach(Some(0));

    let mut asked: Vec<String> = Vec::new();
    let mut ended: Option<(bool, String)> = None;
    while ended.is_none() {
        let msg = tokio::time::timeout(SESSION_WAIT, run.msgs.recv())
            .await
            .expect("the job reported within the bound")
            .expect("the job's channel stays open until the run ends");
        if let Msg::DogJobDone { ok, output, .. } = &msg {
            ended = Some((*ok, output.clone()));
        }
        app.update(msg);
        while let Some(tag) = grant_tag(&app) {
            asked.push(grant_question(&app));
            app.note_modal_reach(Some(0));
            assert_eq!(
                app.update(Msg::Key(KeyPress::Escape)),
                Effect::ModalAnswered { tag, yes: false }
            );
            app.answer_grant(tag, false);
        }
    }

    let (ok, output) = ended.expect("the job ended");
    assert_eq!(
        asked,
        [format!(
            "Replace adopted otel at {} with the catalogue build?",
            adopted.display()
        )],
        "the one question names the path"
    );
    assert!(
        !ok,
        "a declined swap is not an install that landed: {output}"
    );
    assert!(output.contains("nothing was changed"), "{output}");
    assert_eq!(
        std::fs::read(&world.paths.daemon_config).expect("the config"),
        before
    );
    assert_eq!(
        std::fs::read_to_string(&adopted).expect("the adopted file"),
        "old binary"
    );
    assert!(
        !world.paths.dogs_dir.join("otel").exists(),
        "nothing was built"
    );
}

/// How long the stub cargo may take to record its pid, and how long a build
/// the run ended may take to show itself as stopped.
const TAKEN_ATTEMPTS: usize = 1_500;

const STOPPED_ATTEMPTS: usize = 100;

const BUILD_POLL: Duration = Duration::from_millis(20);

/// The pid the stub cargo recorded, once it has recorded one.
#[track_caller]
fn recorded_pid(pid_file: &Path) -> u32 {
    for _ in 0..TAKEN_ATTEMPTS {
        if let Ok(written) = std::fs::read_to_string(pid_file) {
            return written.trim().parse().expect("the stub wrote a pid");
        }
        std::thread::sleep(BUILD_POLL);
    }
    panic!("the stub cargo never started");
}

/// The handle a quit holds is the thread that holds the `cargo`, so ending a
/// run stops the build instead of leaving it to a thread a process exit is
/// about to kill without running its destructors.
///
/// No `start_paused`: the run is a real thread running a real `cargo`.
#[test]
fn ending_a_run_stops_the_build_and_waits_for_the_thread_holding_it() {
    // Short, not the default `$TMPDIR`: a long `$SHEP_HOME` overflows
    // `SUN_LEN` for the control socket path this home names.
    let dir = tempfile::Builder::new()
        .prefix("s")
        .tempdir()
        .expect("a temp home");
    let paths = ShepPaths::resolve(&|_| None, dir.path());
    let (bin, pid_file) = (dir.path().join("bin"), dir.path().join("cargo.pid"));
    std::fs::create_dir_all(&bin).expect("the stub bin");
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, sleeping_cargo(&pid_file)).expect("the stub cargo");
    std::fs::set_permissions(&cargo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("an executable stub");
    crate::dog_index::write_cache(
        &paths.dog_index_cache,
        &crate::dog_index::index_url(),
        serde_json::json!({
            "version": 1,
            "dogs": [{
                "name": "Otel", "package": "shep-otel", "adopt_as": "otel",
                "description": "exports traces",
                "repo": "https://github.com/shep-pm/shep-otel",
                "license": "MIT", "category": "metrics",
                "source": { "kind": "cargo", "version": "0.2.4" }
            }]
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a cached index");

    let mut run = DogRunner::Stub(cargo).start(
        &paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    let pid = recorded_pid(&pid_file);

    assert!(run.end(), "the job's own thread ended its run");

    let mut stopped = false;
    for _ in 0..STOPPED_ATTEMPTS {
        if is_gone(pid) {
            stopped = true;
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(stopped, "cargo {pid} is still building after the run ended");
}

/// A quit while the session is asking a grant question: the replies the
/// dashboard holds go first, or the session stays parked in its callback and
/// the run never ends to be waited for.
///
/// No `start_paused`: the run is a real thread with a real callback.
#[test]
fn a_quit_while_a_grant_is_asked_ends_the_run_that_is_waiting_on_it() {
    let world = World::new();
    let (mut app, _) = installing();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );
    app.note_modal_reach(Some(0));

    // The reports drained into the dashboard the way the loop drains them,
    // until it is holding a question the session is blocked on.
    let mut asked = false;
    for _ in 0..TAKEN_ATTEMPTS {
        while let Ok(msg) = run.msgs.try_recv() {
            app.update(msg);
        }
        asked = matches!(app.modal(), Some(Modal::Grant { .. }));
        if asked {
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(asked, "the session never asked a grant question");

    app.release_dog_grants();
    assert!(
        run.end(),
        "the session left its callback and ended its own run"
    );
    assert!(
        !world.paths.dogs_dir.join("otel/receipt.json").exists(),
        "a run cut off mid-question adopted a dog"
    );
}

/// The wait is bounded: a session parked on a question nobody will answer
/// cannot hold the caller past [`END_BUDGET`].
#[test]
fn ending_a_run_waits_no_longer_than_its_budget() {
    let world = World::new();
    let mut run = DogRunner::Stub(world.cargo.clone()).start(
        &world.paths,
        Job::Install {
            adopt_as: "otel".to_owned(),
        },
    );

    // The question, held here and never answered: the session is parked in a
    // callback, which is the one thing a cancel cannot interrupt.
    let mut held: Option<Arc<GrantReply>> = None;
    for _ in 0..TAKEN_ATTEMPTS {
        if let Ok(Msg::DogGrant { reply, .. }) = run.msgs.try_recv() {
            held = Some(reply);
            break;
        }
        std::thread::sleep(BUILD_POLL);
    }
    assert!(held.is_some(), "the session never asked a grant question");

    let asked_at = Instant::now();
    assert!(!run.end(), "a parked session ended its run");
    assert!(
        asked_at.elapsed() >= END_BUDGET,
        "the wait gave up after {:?}",
        asked_at.elapsed()
    );
}
